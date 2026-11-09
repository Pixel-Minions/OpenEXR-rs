// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Writing a deep part: the C++ library's `DeepScanLineOutputFile` and `DeepTiledOutputFile`.
//!
//! The C++ library composes a deep chunk itself and has Core compress it
//! (src/lib/OpenEXR/ImfCompressor.cpp:99-161 @ v3.5.3): the sample count table, cumulative per row, stored
//! little-endian and compressed as a plain buffer, kept raw where that does not make it
//! smaller; then the samples, line by line, channel by channel, pixel by pixel, compressed
//! with the table at hand, kept raw likewise. The chunk goes to the file through Core's raw
//! deep chunk writers. A tiled part keeps a raw table at the size of a whole tile, as its
//! reused table buffer holds it (`DeepTiledOutputFile`, `TileBufferTask::execute`,
//! src/lib/OpenEXR/ImfDeepTiledOutputFile.cpp:940-985 @ v3.5.3): past an edge tile's own
//! table, the bytes the tiles composed before it left there.

use std::io::{Seek, Write};

use openexr_core::Context;
use openexr_core::compression::compress_chunk;
use openexr_core::decoding::Aliased;
use openexr_core::openexr_attr as ca;

use super::tiles::TileData;
use super::{OutputPart, PartState, core, invalid};
use crate::attribute::{Box2i, LineOrder, Storage, V2};
use crate::deep::DeepFrameBuffer;
use crate::error::{Error, ErrorCode, Result};
use crate::header::Header;

/// A deep chunk as it goes to the file: its sample count table and samples, each compressed
/// or raw, and the samples' unpacked size.
pub(in crate::output) struct DeepChunk {
    pub(in crate::output) table: Vec<u8>,
    pub(in crate::output) data: Vec<u8>,
    pub(in crate::output) unpacked: u64,
}

/// The lines of a deep scanline chunk given so far: each line's sample counts and, per
/// channel, its samples.
pub(in crate::output) struct DeepLines {
    first: i32,
    last: i32,
    lines: Vec<(Vec<u32>, Vec<Vec<u8>>)>,
}

/// Port of the C++ library's use of a `Compressor` on a deep chunk through Core
/// (`compress`/`compressTile` and `compressWithSampleCountTable`,
/// src/lib/OpenEXR/ImfCompressor.cpp:99-161 @ v3.5.3): `input` compressed for the chunk of `range` at
/// `level` (with `table`, the raw sample count table, for the samples), as `exr_compress_chunk`
/// gives it; `None` without a compressor.
fn compress(
    ctx: &mut Context,
    p: i32,
    storage: Storage,
    range: &Box2i,
    level: (i32, i32),
    input: &[u8],
    table: Option<&[u8]>,
) -> Result<Option<Vec<u8>>> {
    // newCompressor gives no compressor for NO_COMPRESSION: the bytes stay raw
    // (src/lib/OpenEXR/ImfCompressor.cpp:328-414 @ v3.5.3)
    if ctx.get_compression(p).map_err(core)? == ca::Compression::NONE {
        return Ok(None);
    }
    // nothing to compress: size 0, which never wins over the raw bytes
    // (Compressor::runEncodeStep, src/lib/OpenEXR/ImfCompressor.cpp:183-188 @ v3.5.3)
    if input.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let bx = ca::Box2i {
        min: ca::V2i {
            x: range.min.x,
            y: range.min.y,
        },
        max: ca::V2i {
            x: range.max.x,
            y: range.max.y,
        },
    };
    let mut cinfo = ctx
        .chunk_default_initialize(p, &bx, level.0, level.1)
        .map_err(core)?;
    cinfo.type_ = match storage {
        Storage::DeepTiled => ca::Storage::DEEP_TILED.0,
        _ => ca::Storage::DEEP_SCANLINE.0,
    } as u8;
    let mut enc = ctx.encoding_initialize(p, &cinfo).map_err(core)?;
    enc.packed_buffer = Some(input.to_vec());
    enc.packed_bytes = input.len() as u64;
    if let Some(t) = table {
        enc.sample_count_table = Some(t.to_vec());
    }
    enc.skip_sample_count_table_compression = table.is_some();
    let r = compress_chunk(&mut enc, ctx).map_err(core);
    let out = match &enc.compressed_buffer {
        Aliased::Own(b) => b[..enc.compressed_bytes.min(b.len())].to_vec(),
        Aliased::Packed => input[..enc.compressed_bytes.min(input.len())].to_vec(),
        Aliased::None => Vec::new(),
    };
    ctx.encoding_destroy(&mut enc).map_err(core)?;
    r?;
    Ok(Some(out))
}

/// One line of a deep part from `fb`: its pixels' sample counts over the data window and,
/// per channel of `h`, their samples (zeros for a channel without a slice: `fillChannelWithZeroes`).
fn deep_line(
    h: &Header,
    fb: &DeepFrameBuffer<'_>,
    x1: i32,
    x2: i32,
    y: i32,
) -> Result<(Vec<u32>, Vec<Vec<u8>>)> {
    let mut counts = Vec::with_capacity((x2 - x1 + 1) as usize);
    for x in x1..=x2 {
        counts.push(fb.count_at(x, y).ok_or_else(|| {
            invalid(format!(
                "Pixel ({x}, {y}) is outside the frame buffer's window"
            ))
        })?);
    }
    let mut data = Vec::new();
    for c in h.channels() {
        let mut line = Vec::new();
        for (x, &n) in (x1..=x2).zip(&counts) {
            match fb.samples_at(&c.name, x, y) {
                Some((_, s)) => line.extend_from_slice(s),
                None => line.resize(line.len() + n as usize * c.pixel_type.size(), 0),
            }
        }
        data.push(line);
    }
    Ok((counts, data))
}

/// A deep chunk's raw sample count table: per row, the running count of samples, as
/// little-endian `int`s.
fn raw_table(lines: &[(Vec<u32>, Vec<Vec<u8>>)]) -> Vec<u8> {
    let mut t = Vec::new();
    for (counts, _) in lines {
        let mut acc = 0i32;
        for &n in counts {
            acc = acc.wrapping_add(n as i32);
            t.extend_from_slice(&acc.to_le_bytes());
        }
    }
    t
}

impl<W: Write + Seek + Send + 'static> OutputPart<'_, W> {
    /// Checks `fb`'s slices against the part's channels (`setFrameBuffer`,
    /// src/lib/OpenEXR/ImfDeepScanLineOutputFile.cpp:1046-1172 @ v3.5.3).
    fn check_deep(&self, fb: &DeepFrameBuffer<'_>, want: Storage) -> Result<()> {
        if self.file.parts[..self.index]
            .iter()
            .any(PartState::incomplete)
        {
            return Err(Error::new(
                ErrorCode::IncorrectPart,
                openexr_core::base::get_default_error_message(ErrorCode::IncorrectPart.code()),
            ));
        }
        let h = &self.file.headers[self.index];
        if h.storage() != want {
            return Err(invalid(match want {
                Storage::DeepScanline => "write_deep_pixels writes deep scanline parts",
                _ => "write_deep_tiles writes deep tiled parts",
            }));
        }
        let w = fb.window();
        for c in h.channels() {
            if let Some((t, _)) = fb.samples_at(&c.name, w.min.x, w.min.y)
                && t != c.pixel_type
            {
                return Err(invalid(format!(
                    "Pixel type of \"{}\" channel is not compatible with the frame buffer's pixel type.",
                    String::from_utf8_lossy(&c.name)
                )));
            }
        }
        Ok(())
    }

    /// Writes the next `lines` lines of a deep scanline part from `fb`, in the part's line
    /// order: port of `DeepScanLineOutputFile::writePixels`
    /// (src/lib/OpenEXR/ImfDeepScanLineOutputFile.cpp:1184-1428 @ v3.5.3), a chunk composed and written once
    /// all its lines are given.
    pub fn write_deep_pixels(&mut self, fb: &DeepFrameBuffer<'_>, lines: i32) -> Result<()> {
        self.check_deep(fb, Storage::DeepScanline)?;
        let state = &self.file.parts[self.index];
        if i64::from(lines) > state.lines_left {
            return Err(invalid(
                "Tried to write more scan lines than specified by the data window.",
            ));
        }
        let p = self.index as i32;
        for _ in 0..lines.max(0) {
            let file = &mut *self.file;
            let ctx = file.ctx.as_mut().expect("not finished");
            let h = &file.headers[self.index];
            let dw = h.data_window();
            let state = &mut file.parts[self.index];
            let y = state.next_y;
            if state.deep_lines.is_none() {
                let lpc = ctx.get_scanlines_per_chunk(p).map_err(core)?;
                let first = dw.min.y + (y - dw.min.y) / lpc * lpc;
                state.deep_lines = Some(DeepLines {
                    first,
                    last: (first + lpc - 1).min(dw.max.y),
                    lines: Vec::new(),
                });
            }
            let line = deep_line(h, fb, dw.min.x, dw.max.x, y)?;
            let staged = state.deep_lines.as_mut().expect("staged");
            staged.lines.push(line);
            state.next_y += state.step;
            state.lines_left -= 1;
            if staged.lines.len() as i32 == staged.last - staged.first + 1 {
                let mut staged = state.deep_lines.take().expect("staged");
                // the lines in file order, top to bottom
                if state.step < 0 {
                    staged.lines.reverse();
                }
                let table = raw_table(&staged.lines);
                let data: Vec<u8> = staged
                    .lines
                    .iter()
                    .flat_map(|(_, chans)| chans.iter().flatten().copied())
                    .collect();
                // compressWithSampleCountTable's range: the data window's columns, a whole
                // chunk's lines from its first (src/lib/OpenEXR/ImfCompressor.cpp:107-124
                // @ v3.5.3)
                let lpc = ctx.get_scanlines_per_chunk(p).map_err(core)?;
                let range = Box2i {
                    min: V2 {
                        x: dw.min.x,
                        y: staged.first,
                    },
                    max: V2 {
                        x: dw.max.x,
                        y: staged.first + lpc - 1,
                    },
                };
                let ct = compress(ctx, p, Storage::DeepScanline, &range, (0, 0), &table, None)?;
                let table_out = match ct {
                    Some(c) if c.len() < table.len() => c,
                    _ => table.clone(),
                };
                let cd = compress(
                    ctx,
                    p,
                    Storage::DeepScanline,
                    &range,
                    (0, 0),
                    &data,
                    Some(&table),
                )?;
                let data_out = match cd {
                    Some(c) if c.len() < data.len() => c,
                    _ => data.clone(),
                };
                ctx.write_deep_scanline_chunk(
                    p,
                    staged.first,
                    // no samples: Core takes no pointer for them (NULL, size 0)
                    Some(&data_out[..]).filter(|d| !d.is_empty()),
                    data.len() as u64,
                    Some(&table_out),
                )
                .map_err(core)?;
            }
        }
        Ok(())
    }

    /// Writes tile `(dx, dy)` of level `level` of a deep tiled part from `fb`
    /// ([`write_deep_tiles`](Self::write_deep_tiles)).
    pub fn write_deep_tile(
        &mut self,
        fb: &DeepFrameBuffer<'_>,
        dx: i32,
        dy: i32,
        level: (i32, i32),
    ) -> Result<()> {
        self.write_deep_tiles(fb, (dx, dx), (dy, dy), level)
    }

    /// Writes tiles `dx` by `dy` of `level` of a deep tiled part from `fb`, whose window is
    /// in the level's coordinates: port of `DeepTiledOutputFile::writeTiles`
    /// (src/lib/OpenEXR/ImfDeepTiledOutputFile.cpp:1489-1696 @ v3.5.3). Each tile is composed
    /// and compressed when given, and goes to the file in the part's tile order.
    pub fn write_deep_tiles(
        &mut self,
        fb: &DeepFrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        self.check_deep(fb, Storage::DeepTiled)?;
        let p = self.index as i32;
        let (lx, ly) = level;
        let tiles = self.file.parts[self.index].tiles.as_ref().expect("tiled");
        if !tiles.valid_tile(dx.0, dy.0, lx, ly) || !tiles.valid_tile(dx.1, dy.1, lx, ly) {
            return Err(invalid("Tile coordinates are invalid."));
        }
        let decreasing = tiles.order == LineOrder::DecreasingY;
        let dx = if dx.1 < dx.0 { (dx.1, dx.0) } else { dx };
        let dy = if dy.1 < dy.0 { (dy.1, dy.0) } else { dy };
        let rows: Vec<i32> = if decreasing {
            (dy.0..=dy.1).rev().collect()
        } else {
            (dy.0..=dy.1).collect()
        };
        for ty in rows {
            for tx in dx.0..=dx.1 {
                let file = &mut *self.file;
                let ctx = file.ctx.as_mut().expect("not finished");
                let h = &file.headers[self.index];
                let dw = h.data_window();
                let (tw, th) = ctx.get_tile_sizes(p, lx, ly).map_err(core)?;
                let (lw, lh) = ctx.get_level_sizes(p, lx, ly).map_err(core)?;
                let x0 = dw.min.x + tx * tw;
                let y0 = dw.min.y + ty * th;
                let range = Box2i {
                    min: V2 { x: x0, y: y0 },
                    max: V2 {
                        x: (x0 + tw - 1).min(dw.min.x + lw - 1),
                        y: (y0 + th - 1).min(dw.min.y + lh - 1),
                    },
                };
                let lines = (range.min.y..=range.max.y)
                    .map(|y| deep_line(h, fb, range.min.x, range.max.x, y))
                    .collect::<Result<Vec<_>>>()?;
                let table = raw_table(&lines);
                let data: Vec<u8> = lines
                    .iter()
                    .flat_map(|(_, chans)| chans.iter().flatten().copied())
                    .collect();
                let state = &mut file.parts[self.index];
                let max = state.deep_table.len();
                if table.len() > max {
                    return Err(invalid(
                        "A tile's sample count table outgrows a whole tile's",
                    ));
                }
                state.deep_table[..table.len()].copy_from_slice(&table);
                let ct = compress(ctx, p, Storage::DeepTiled, &range, level, &table, None)?;
                let table_out = match ct {
                    Some(c) if c.len() < max => c,
                    _ => state.deep_table.clone(),
                };
                let cd = compress(
                    ctx,
                    p,
                    Storage::DeepTiled,
                    &range,
                    level,
                    &data,
                    Some(&table),
                )?;
                let data_out = match cd {
                    Some(c) if c.len() < data.len() => c,
                    _ => data.clone(),
                };
                let chunk = DeepChunk {
                    table: table_out,
                    data: data_out,
                    unpacked: data.len() as u64,
                };
                self.put_tile((tx, ty, lx, ly), TileData::Deep(chunk))?;
            }
        }
        Ok(())
    }
}
