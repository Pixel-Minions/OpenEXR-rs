// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Reading a flat part's pixels into a [`FrameBuffer`]: the C++ library's
//! `ScanLineInputFile`, `TiledInputFile` and `InputFile::readPixels` (for a tiled part), over
//! Core's decoder, as those drive it.
//!
//! Every slice's samples are checked to fit their buffer once, before Core is called, so that
//! a read either fails up front or writes only where the caller asked. Where the C++ library
//! writes a missing channel's fill value one row early (a y-sampled fill slice whose read
//! starts between samples: `ScanLineProcess::run_fill`, src/lib/OpenEXR/
//! ImfScanLineInputFile.cpp:663-721 @ v3.5.3, starts at row `fbY / ySampling` for the first
//! sampled line after `fbY`), each fill sample goes where its slice says.

use openexr_core::Context;
use openexr_core::chunk_info::ChunkInfo;
use openexr_core::coding::BufferRef;
use openexr_core::decoding::DecodePipeline;

use crate::attribute::{Box2i, LevelMode, PixelType, Storage, V2};
use crate::error::{Error, ErrorCode, Result};
use crate::frame_buffer::{FrameBuffer, Slice, first_multiple, sample_count};
use crate::header::Header;
use crate::input::MultiPartInputFile;

/// One part of a file open to read: the C++ library's `InputPart`.
#[derive(Debug, Clone, Copy)]
pub struct InputPart<'f> {
    file: &'f MultiPartInputFile,
    index: usize,
}

fn core(e: openexr_core::Error) -> Error {
    Error::from_core(&e)
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, msg)
}

impl MultiPartInputFile {
    /// Part `index`.
    pub fn part(&self, index: usize) -> Result<InputPart<'_>> {
        if index >= self.parts() {
            return Err(Error::new(
                ErrorCode::ArgumentOutOfRange,
                format!("Part index ({index}) out of range"),
            ));
        }
        Ok(InputPart { file: self, index })
    }
}

impl<'f> InputPart<'f> {
    fn ctx(&self) -> &'f Context {
        self.file.context()
    }

    fn p(&self) -> i32 {
        self.index as i32
    }

    /// The part's number in its file.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The part's header.
    pub fn header(&self) -> &'f Header {
        &self.file.headers()[self.index]
    }

    /// Reads lines `y1` to `y2` (in either order, both included) of the part into `fb`: a
    /// scanline part's chunks, or a tiled part's level 0 tiles (as the C++ library's
    /// `InputFile` reads a tiled part). Channels of `fb` the part lacks get their slice's
    /// fill value; channels of the part `fb` lacks are skipped.
    pub fn read_pixels(&self, fb: &mut FrameBuffer<'_>, y1: i32, y2: i32) -> Result<()> {
        let (y1, y2) = if y2 < y1 { (y2, y1) } else { (y1, y2) };
        let dw = self.header().data_window();
        if y1 < dw.min.y || y2 > dw.max.y {
            // ScanLineInputFile::Data::readPixels (ImfScanLineInputFile.cpp:416-447 @ v3.5.3)
            return Err(invalid(format!(
                "Tried to read scan line outside the image file's data window: {y1} - {y2} vs datawindow {} - {}",
                dw.min.y, dw.max.y
            )));
        }
        match self.header().storage() {
            Storage::Scanline => self.read_scanlines(fb, dw, y1, y2),
            Storage::Tiled => {
                // InputFile::Data::lockedSetFrameBuffer (src/lib/OpenEXR/ImfInputFile.cpp:
                // 436-445 @ v3.5.3): refuses a tiled part whose chunk table Core does not
                // validate, once a frame buffer names a channel
                if fb.slices().next().is_some() {
                    self.ctx().validate_chunk_table(self.p()).map_err(core)?;
                }
                self.buffered_read_pixels(fb, dw, y1, y2)
            }
            Storage::DeepScanline | Storage::DeepTiled => Err(invalid(
                "Deep parts are read with a DeepFrameBuffer, not a FrameBuffer",
            )),
            Storage::Unknown => Err(invalid("The part's type is unknown")),
        }
    }

    /// Reads tiles `dx` (first, last) by `dy` of level `(lx, ly)` into `fb`, in that level's
    /// coordinates (the level's data window starts where the part's does).
    pub fn read_tiles(
        &self,
        fb: &mut FrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        if !self.is_valid_level(level.0, level.1) {
            return Err(invalid(format!(
                "Level coordinate ({}, {}) is invalid.",
                level.0, level.1
            )));
        }
        let dx = if dx.1 < dx.0 { (dx.1, dx.0) } else { dx };
        let dy = if dy.1 < dy.0 { (dy.1, dy.0) } else { dy };
        self.read_tile_range(fb, dx, dy, level)
    }

    /// Reads tile `(dx, dy)` of level `(lx, ly)` into `fb`.
    pub fn read_tile(
        &self,
        fb: &mut FrameBuffer<'_>,
        dx: i32,
        dy: i32,
        level: (i32, i32),
    ) -> Result<()> {
        self.read_tiles(fb, (dx, dx), (dy, dy), level)
    }

    fn tiled(&self) -> Result<()> {
        if !self.header().storage().is_tiled() {
            return Err(invalid("The part is not tiled"));
        }
        Ok(())
    }

    /// Core's context, for the deep readers.
    pub(crate) fn core_context(&self) -> &'f Context {
        self.ctx()
    }

    /// The number of levels in x and y (1 and 1 for a part of one level).
    pub fn num_levels(&self) -> Result<(i32, i32)> {
        self.tiled()?;
        self.ctx().get_tile_levels(self.p()).map_err(core)
    }

    /// Port of `TiledInputFile::isValidLevel` (src/lib/OpenEXR/ImfTiledInputFile.cpp:457-467
    /// @ v3.5.3).
    pub fn is_valid_level(&self, lx: i32, ly: i32) -> bool {
        let Ok((nx, ny)) = self.num_levels() else {
            return false;
        };
        if lx < 0 || ly < 0 {
            return false;
        }
        let mipmap = self.header().tiles().map(|t| t.level_mode) == Some(LevelMode::Mipmap);
        if mipmap && lx != ly {
            return false;
        }
        lx < nx && ly < ny
    }

    /// The width and height of level `(lx, ly)`.
    pub fn level_size(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        self.tiled()?;
        self.ctx()
            .get_level_sizes(self.p(), level.0, level.1)
            .map_err(core)
    }

    /// The data window of level `(lx, ly)`: the part's, at the level's size.
    pub fn data_window_for_level(&self, level: (i32, i32)) -> Result<Box2i> {
        let (w, h) = self.level_size(level)?;
        let min = self.header().data_window().min;
        Ok(Box2i {
            min,
            max: V2 {
                x: min.x + w - 1,
                y: min.y + h - 1,
            },
        })
    }

    /// The number of tiles of level `(lx, ly)` in x and y.
    pub fn num_tiles(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        self.tiled()?;
        self.ctx()
            .get_tile_counts(self.p(), level.0, level.1)
            .map_err(core)
    }

    /// The width and height of the tiles of level `(lx, ly)`.
    pub fn tile_size(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        self.tiled()?;
        self.ctx()
            .get_tile_sizes(self.p(), level.0, level.1)
            .map_err(core)
    }

    /// The pixels of tile `(dx, dy)` of level `(lx, ly)`, in the level's coordinates.
    pub fn data_window_for_tile(&self, dx: i32, dy: i32, level: (i32, i32)) -> Result<Box2i> {
        let lw = self.data_window_for_level(level)?;
        let (tw, th) = self.tile_size(level)?;
        let (nx, ny) = self.num_tiles(level)?;
        if dx < 0 || dy < 0 || dx >= nx || dy >= ny {
            return Err(invalid("Arguments not in valid range."));
        }
        let min = V2 {
            x: lw.min.x + dx * tw,
            y: lw.min.y + dy * th,
        };
        Ok(Box2i {
            min,
            max: V2 {
                x: (min.x + tw - 1).min(lw.max.x),
                y: (min.y + th - 1).min(lw.max.y),
            },
        })
    }

    /// Port of `ScanLineInputFile::Data::readPixels`
    /// (src/lib/OpenEXR/ImfScanLineInputFile.cpp:416-520 @ v3.5.3): each chunk of the lines
    /// a unit, decoded on the file's threads (`LineBufferTask`, lines 522-545) or one after
    /// another.
    fn read_scanlines(&self, fb: &mut FrameBuffer<'_>, dw: Box2i, y1: i32, y2: i32) -> Result<()> {
        let ctx = self.ctx();
        let p = self.p();
        let fills = self.check_frame_buffer(fb, dw.min.x, dw.max.x, y1, y2)?;
        let spc = ctx.get_scanlines_per_chunk(p).map_err(core)?;
        let mut units = Vec::new();
        let mut y = y1;
        while y <= y2 {
            let cinfo = ctx.read_scanline_chunk_info(p, y).map_err(core)?;
            let end = i64::from(cinfo.start_y) + i64::from(cinfo.height) - 1;
            let ignore = if i64::from(y2) < end {
                (end - i64::from(y2)) as i32
            } else {
                0
            };
            let last = cinfo.start_y + cinfo.height - 1 - ignore;
            units.push(Unit {
                jobs: vec![Job {
                    cinfo,
                    at: (cinfo.start_x, y),
                    skip: y - cinfo.start_y,
                    ignore,
                }],
                cols: (dw.min.x, dw.max.x),
                rows: (y, last),
            });
            y += spc - (y - cinfo.start_y);
        }
        self.decode_units(fb, &fills, units)
    }

    /// Lines `y1..=y2` of a tiled part's level 0: port of `InputFile::Data::
    /// bufferedReadPixels` (src/lib/OpenEXR/ImfInputFile.cpp:537-618 @ v3.5.3). The whole
    /// part is read tile by tile into `fb`; part of it, a row of tiles at a time into a
    /// buffer of the slices' types (`lockedSetFrameBuffer`'s cached frame buffer, lines
    /// 413-466), whose lines in the range are copied to `fb` (`copyCachedBuffer`, lines
    /// 620-678) and the slices the part lacks filled (`fillBuffer`, lines 680-741). Core's
    /// own line skips cannot stand in for the buffer: an uncompressed tile read straight to
    /// the caller ignores them (docs/improvements.md, I-34).
    fn buffered_read_pixels(
        &self,
        fb: &mut FrameBuffer<'_>,
        dw: Box2i,
        y1: i32,
        y2: i32,
    ) -> Result<()> {
        let (_, tsy) = self.tile_size((0, 0))?;
        let (ntx, _) = self.num_tiles((0, 0))?;
        let dy1 = (y1 - dw.min.y) / tsy;
        let dy2 = (y2 - dw.min.y) / tsy;
        if y1 == dw.min.y && y2 == dw.max.y {
            return self.read_tile_range(fb, (0, ntx - 1), (dy1, dy2), (0, 0));
        }
        let fills = self.check_frame_buffer(fb, dw.min.x, dw.max.x, y1, y2)?;
        no_subsampling(fb)?;
        let w = dw.width() as usize;
        let cached: Vec<(Vec<u8>, Slice)> = fb
            .slices()
            .filter(|(n, _)| self.header().channel(n).is_some())
            .map(|(n, s)| (n.to_vec(), *s))
            .collect();
        for j in dy1..=dy2 {
            let tile = self.data_window_for_tile(0, j, (0, 0))?;
            let rows = tile.height() as usize;
            let mut bufs: Vec<Vec<u8>> = cached
                .iter()
                .map(|(_, s)| vec![0u8; rows * w * s.sample_size()])
                .collect();
            if !cached.is_empty() {
                let mut cache = FrameBuffer::new();
                for ((name, s), buf) in cached.iter().zip(bufs.iter_mut()) {
                    let size = s.sample_size() as i64;
                    let b = cache.add_bytes(buf);
                    let origin = V2 {
                        x: dw.min.x,
                        y: tile.min.y,
                    };
                    let slice = Slice::new(s.pixel_type, b, 0, origin, size, size * w as i64);
                    cache.insert(name, slice)?;
                }
                self.read_tile_range(&mut cache, (0, ntx - 1), (j, j), (0, 0))?;
            }
            let (a, b) = (y1.max(tile.min.y), y2.min(tile.max.y));
            for ((_, s), buf) in cached.iter().zip(&bufs) {
                let size = s.sample_size();
                let out = fb.buffer_mut(s.buffer)?;
                for y in a..=b {
                    let row = (y - tile.min.y) as usize * w * size;
                    for x in dw.min.x..=dw.max.x {
                        let from = row + (x - dw.min.x) as usize * size;
                        // checked by check_frame_buffer
                        let to = s.offset_of(x, y).expect("checked") as usize;
                        out[to..to + size].copy_from_slice(&buf[from..from + size]);
                    }
                }
            }
            for name in &fills {
                let s = *fb.slice(name).expect("a slice of fb");
                fill(
                    fb.buffer_mut(s.buffer)?,
                    0,
                    &s,
                    (dw.min.x, dw.max.x),
                    (a, b),
                )?;
            }
        }
        Ok(())
    }

    /// Reads tiles `dx` by `dy` of `level`: port of `TiledInputFile::Data::readTiles`
    /// (src/lib/OpenEXR/ImfTiledInputFile.cpp:763-838 @ v3.5.3), each row of tiles a unit,
    /// decoded on the file's threads (`TileBufferTask`, lines 840-861) or one after another.
    fn read_tile_range(
        &self,
        fb: &mut FrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        let ctx = self.ctx();
        let p = self.p();
        let first = self.data_window_for_tile(dx.0, dy.0, level)?;
        let last = self.data_window_for_tile(dx.1, dy.1, level)?;
        let fills =
            self.check_frame_buffer(fb, first.min.x, last.max.x, first.min.y, last.max.y)?;
        no_subsampling(fb)?;
        let (tw, th) = self.tile_size(level)?;
        let dw = self.header().data_window();
        let mut units = Vec::new();
        for ty in dy.0..=dy.1 {
            let row = self.data_window_for_tile(dx.0, ty, level)?;
            let mut jobs = Vec::new();
            for tx in dx.0..=dx.1 {
                let cinfo = ctx
                    .read_tile_chunk_info(p, tx, ty, level.0, level.1)
                    .map_err(core)?;
                let x0 = dw.min.x + tw * cinfo.start_x;
                let y0 = dw.min.y + th * cinfo.start_y;
                jobs.push(Job {
                    cinfo,
                    at: (x0, y0),
                    skip: 0,
                    ignore: 0,
                });
            }
            units.push(Unit {
                jobs,
                cols: (first.min.x, last.max.x),
                rows: (row.min.y, row.max.y),
            });
        }
        self.decode_units(fb, &fills, units)
    }

    /// The slices of `fb` the part lacks (to fill), after checking that every slice's samples
    /// for columns `x1..=x2` and lines `y1..=y2` fit its buffer and that each channel the part
    /// has is sampled as its slice (`ScanLineInputFile::setFrameBuffer`,
    /// src/lib/OpenEXR/ImfScanLineInputFile.cpp:243-280 @ v3.5.3).
    fn check_frame_buffer(
        &self,
        fb: &FrameBuffer<'_>,
        x1: i32,
        x2: i32,
        y1: i32,
        y2: i32,
    ) -> Result<Vec<Vec<u8>>> {
        let header = self.header();
        let mut fills = Vec::new();
        for (name, s) in fb.slices() {
            match header.channel(name) {
                None => fills.push(name.to_vec()),
                Some(c) if c.x_sampling != s.x_sampling || c.y_sampling != s.y_sampling => {
                    return Err(invalid(format!(
                        "X and/or y subsampling factors of \"{}\" channel are not compatible with the frame buffer's subsampling factors.",
                        String::from_utf8_lossy(name)
                    )));
                }
                Some(_) => {}
            }
            if !fb.writable(s.buffer) {
                return Err(crate::frame_buffer::read_only(s.buffer));
            }
            check_slice(fb, name, s, x1, x2, y1, y2)?;
        }
        Ok(fills)
    }

    /// Decodes `units` into `fb`: on the file's threads where it has several and each unit
    /// writes its own bytes of every buffer ([`bands`]), each unit into its share of the
    /// buffers; else one chunk after another. The bytes are the same either way, and so is
    /// the error: the first unit's that fails.
    fn decode_units(
        &self,
        fb: &mut FrameBuffer<'_>,
        fills: &[Vec<u8>],
        units: Vec<Unit>,
    ) -> Result<()> {
        let slices: Vec<(Vec<u8>, Slice)> = fb.slices().map(|(n, s)| (n.to_vec(), *s)).collect();
        let fill_slices: Vec<Slice> = fills
            .iter()
            .map(|n| *fb.slice(n).expect("a slice of fb"))
            .collect();
        let nbuf = fb.buffer_count();
        let pool = self.file.pool();
        if pool.is_parallel()
            && units.len() > 1
            && let Some(bands) = bands(&slices, &units, nbuf)
        {
            let mut outs = fb.outputs()?;
            let mut shares: Vec<Vec<&mut [u8]>> = (0..units.len())
                .map(|_| (0..nbuf).map(|_| &mut [][..]).collect())
                .collect();
            let mut bases = vec![vec![0usize; nbuf]; units.len()];
            for (b, out) in outs.iter_mut().enumerate() {
                let mut order: Vec<(usize, (usize, usize))> = bands
                    .iter()
                    .enumerate()
                    .filter_map(|(u, bs)| bs[b].map(|r| (u, r)))
                    .collect();
                order.sort_by_key(|&(_, (lo, _))| lo);
                let mut rest: &mut [u8] = std::mem::take(out);
                let mut consumed = 0;
                for (u, (lo, hi)) in order {
                    let (_, tail) = rest.split_at_mut(lo - consumed);
                    let (share, tail) = tail.split_at_mut(hi - lo);
                    shares[u][b] = share;
                    bases[u][b] = lo;
                    rest = tail;
                    consumed = hi;
                }
            }
            let work: Vec<_> = units.into_iter().zip(shares).zip(bases).collect();
            let results = pool.map(work, |((unit, mut outs), bases)| {
                self.decode_jobs(&slices, &fill_slices, &unit.jobs, &mut outs, &bases)
            });
            return results.into_iter().collect::<Result<Vec<()>>>().map(drop);
        }
        let jobs: Vec<Job> = units.into_iter().flat_map(|u| u.jobs).collect();
        let mut outs = fb.outputs()?;
        self.decode_jobs(&slices, &fill_slices, &jobs, &mut outs, &vec![0; nbuf])
    }

    /// Decodes `jobs` one after another with one decoder, as one C++ process does, into
    /// `outs`, buffer `b` holding the frame buffer's buffer from byte `bases[b]`.
    fn decode_jobs(
        &self,
        slices: &[(Vec<u8>, Slice)],
        fills: &[Slice],
        jobs: &[Job],
        outs: &mut [&mut [u8]],
        bases: &[usize],
    ) -> Result<()> {
        let mut dec = None;
        for job in jobs {
            self.decode_chunk(slices, fills, job, outs, bases, &mut dec)?;
        }
        Ok(())
    }

    /// Decodes one chunk into `outs`, its first kept line `skip` lines down and its last
    /// `ignore` lines up from the chunk's edges, the first kept pixel at `at`; then fills the
    /// slices the part lacks. Port of `ScanLineProcess::run_decode` and `update_pointers`
    /// (src/lib/OpenEXR/ImfScanLineInputFile.cpp:547-661 @ v3.5.3) and of
    /// `TileProcess::run_decode` and `update_pointers`
    /// (src/lib/OpenEXR/ImfTiledInputFile.cpp:863-963 @ v3.5.3), which differ only in where a
    /// chunk starts.
    fn decode_chunk<'c>(
        &self,
        slices: &[(Vec<u8>, Slice)],
        fills: &[Slice],
        job: &Job,
        outs: &mut [&mut [u8]],
        bases: &[usize],
        dec: &mut Option<DecodePipeline<'c>>,
    ) -> Result<()>
    where
        'f: 'c,
    {
        let ctx: &'c Context = self.ctx();
        let p = self.p();
        let Job {
            cinfo,
            at,
            skip,
            ignore,
        } = job;
        let first = dec.is_none();
        match dec {
            None => *dec = Some(ctx.decoding_initialize(p, cinfo).map_err(core)?),
            Some(d) => ctx.decoding_update(p, cinfo, d).map_err(core)?,
        }
        let d = dec.as_mut().expect("initialized");
        d.user_line_begin_skip = *skip;
        d.user_line_end_ignore = *ignore;
        for c in d.channels.iter_mut() {
            let slice = slices
                .iter()
                .find(|(n, _)| *n == c.channel_name)
                .map(|(_, s)| *s);
            let Some(s) = slice.filter(|_| c.height != 0) else {
                c.data = None;
                c.user_pixel_stride = 0;
                c.user_line_stride = 0;
                continue;
            };
            c.user_bytes_per_element = s.sample_size() as i16;
            c.user_data_type = s.pixel_type.value() as u16;
            c.user_pixel_stride = stride(s.x_stride)?;
            c.user_line_stride = stride(s.y_stride)?;
            let offset = s
                .offset_of(at.0, at.1)
                .and_then(|o| usize::try_from(o).ok())
                .and_then(|o| o.checked_sub(bases[s.buffer]))
                .ok_or_else(|| invalid("Slice's first sample is outside its buffer"))?;
            c.data = Some(BufferRef {
                buffer: s.buffer,
                offset,
            });
        }
        if first {
            ctx.decoding_choose_default_routines(p, d).map_err(core)?;
        }
        ctx.decoding_run(p, d, outs).map_err(core)?;
        let rows = (at.1, at.1 + cinfo.height - skip - ignore - 1);
        let cols = (at.0, at.0 + cinfo.width - 1);
        for s in fills {
            fill(outs[s.buffer], bases[s.buffer], s, cols, rows)?;
        }
        Ok(())
    }
}

/// A chunk to decode: its information, its first kept pixel, and its line skips.
struct Job {
    cinfo: ChunkInfo,
    at: (i32, i32),
    skip: i32,
    ignore: i32,
}

/// Chunks one task decodes, one after another, and the pixels they cover (columns and lines,
/// inclusive): a scanline chunk, or a row of tiles (tiles side by side share their lines'
/// bytes).
struct Unit {
    jobs: Vec<Job>,
    cols: (i32, i32),
    rows: (i32, i32),
}

/// A unit's bytes `[lo, hi)` of a buffer, or none.
type Band = Option<(usize, usize)>;

/// The bytes `[lo, hi)` of each buffer each unit writes (every slice's samples of its pixels,
/// and the samples its chunks' pointers name), or `None` where two units' bytes of a buffer
/// meet: then the units run one after another. (A y-sampled channel whose chunk starts
/// between samples has its pointer on the sample row before, which the unit above writes:
/// `update_pointers`' `fbY / ySampling`.)
fn bands(slices: &[(Vec<u8>, Slice)], units: &[Unit], nbuf: usize) -> Option<Vec<Vec<Band>>> {
    let mut bands: Vec<Vec<Band>> = vec![vec![None; nbuf]; units.len()];
    for (u, unit) in units.iter().enumerate() {
        for (_, s) in slices {
            let nx = sample_count(unit.cols.0, unit.cols.1, s.x_sampling);
            let ny = sample_count(unit.rows.0, unit.rows.1, s.y_sampling);
            if nx == 0 || ny == 0 {
                continue;
            }
            let fx = first_multiple(unit.cols.0, s.x_sampling) as i32;
            let fy = first_multiple(unit.rows.0, s.y_sampling) as i32;
            let lx = fx + (nx as i32 - 1) * s.x_sampling;
            let ly = fy + (ny as i32 - 1) * s.y_sampling;
            let size = s.sample_size() as i64;
            let mut lo = i64::MAX;
            let mut hi = i64::MIN;
            let corners = [(fx, fy), (lx, fy), (fx, ly), (lx, ly)];
            let pointers = unit.jobs.iter().map(|j| j.at);
            for (x, y) in corners.into_iter().chain(pointers) {
                let o = s.offset_of(x, y)?;
                lo = lo.min(o);
                hi = hi.max(o + size);
            }
            let (lo, hi) = (usize::try_from(lo).ok()?, usize::try_from(hi).ok()?);
            let band = &mut bands[u][s.buffer];
            *band = Some(match *band {
                None => (lo, hi),
                Some((a, b)) => (a.min(lo), b.max(hi)),
            });
        }
    }
    for b in 0..nbuf {
        let mut spans: Vec<(usize, usize)> = bands.iter().filter_map(|bs| bs[b]).collect();
        spans.sort_unstable();
        if spans.windows(2).any(|w| w[0].1 > w[1].0) {
            return None;
        }
    }
    Some(bands)
}

/// `TileProcess::update_pointers` and `run_fill` (src/lib/OpenEXR/ImfTiledInputFile.cpp:
/// 944-945, 973-974 @ v3.5.3) refuse sampled slices.
fn no_subsampling(fb: &FrameBuffer<'_>) -> Result<()> {
    if fb
        .slices()
        .any(|(_, s)| s.x_sampling != 1 || s.y_sampling != 1)
    {
        return Err(invalid("Tiled data should not have subsampling."));
    }
    Ok(())
}

/// A stride as Core's `int32_t`.
fn stride(v: i64) -> Result<i32> {
    i32::try_from(v).map_err(|_| invalid(format!("Stride {v} does not fit in 32 bits")))
}

/// Checks that slice `s`'s samples of columns `x1..=x2` and lines `y1..=y2` lie in its
/// buffer: the offsets are linear in the sample's column and row, so their extremes are at
/// the corners.
pub(crate) fn check_slice(
    fb: &FrameBuffer<'_>,
    name: &[u8],
    s: &Slice,
    x1: i32,
    x2: i32,
    y1: i32,
    y2: i32,
) -> Result<()> {
    let nx = sample_count(x1, x2, s.x_sampling);
    let ny = sample_count(y1, y2, s.y_sampling);
    if nx == 0 || ny == 0 {
        return Ok(());
    }
    let fx = first_multiple(x1, s.x_sampling) as i32;
    let fy = first_multiple(y1, s.y_sampling) as i32;
    let lx = fx + (nx as i32 - 1) * s.x_sampling;
    let ly = fy + (ny as i32 - 1) * s.y_sampling;
    let len = fb.buffer_len(s.buffer) as i64;
    let size = s.sample_size() as i64;
    for (x, y) in [(fx, fy), (lx, fy), (fx, ly), (lx, ly)] {
        let ok = s
            .offset_of(x, y)
            .is_some_and(|o| o >= 0 && o.checked_add(size).is_some_and(|e| e <= len));
        if !ok {
            return Err(invalid(format!(
                "The slice of channel \"{}\" does not fit its buffer: pixel ({x}, {y}) falls outside its {len} bytes",
                String::from_utf8_lossy(name)
            )));
        }
    }
    Ok(())
}

/// Writes `s`'s fill value to its samples of columns `cols` and lines `rows` (inclusive),
/// in `buf`, which holds the slice's buffer from byte `base`; converted as the C++ library
/// converts it (`float (v)`, `half (v)`: through `float`, then Imath's
/// `imath_float_to_half`; `(unsigned int) (v)` truncates toward zero, and here saturates
/// where C's conversion is undefined).
fn fill(buf: &mut [u8], base: usize, s: &Slice, cols: (i32, i32), rows: (i32, i32)) -> Result<()> {
    let mut bytes = [0u8; 4];
    let n = match s.pixel_type {
        PixelType::Half => {
            let h = openexr_core::imath_half::imath_float_to_half(s.fill_value as f32);
            bytes[..2].copy_from_slice(&h.to_ne_bytes());
            2
        }
        PixelType::Float => {
            bytes.copy_from_slice(&(s.fill_value as f32).to_ne_bytes());
            4
        }
        _ => {
            bytes.copy_from_slice(&(s.fill_value as u32).to_ne_bytes());
            4
        }
    };
    for y in rows.0..=rows.1 {
        if y % s.y_sampling != 0 {
            continue;
        }
        for x in cols.0..=cols.1 {
            if x % s.x_sampling != 0 {
                continue;
            }
            // checked by check_slice, and within the unit's share by `bands`
            let o = s
                .offset_of(x, y)
                .and_then(|o| usize::try_from(o).ok())
                .and_then(|o| o.checked_sub(base))
                .filter(|&o| o + n <= buf.len())
                .ok_or_else(|| invalid("A fill sample is outside its buffer"))?;
            buf[o..o + n].copy_from_slice(&bytes[..n]);
        }
    }
    Ok(())
}
