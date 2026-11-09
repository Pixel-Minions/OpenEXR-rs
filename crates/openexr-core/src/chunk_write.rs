// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The write half of `chunk.c` (src/lib/OpenEXRCore/chunk.c:639-675, 704-849, 1676-2428
//! @ v3.5.3): `alloc_chunk_table`, `exr_chunk_default_initialize`, the chunk info and chunk
//! writes, and `internal_validate_next_chunk`.
//!
//! A chunk's bytes are slices; `None` stands for upstream's NULL pointer. The messages that
//! print a NULL pointer with `%p` print it as the C runtime does; one printed for a non-NULL
//! pointer (sample data of size 0 for a deep chunk) prints this process's address, which
//! cannot match upstream's.

use crate::attributes::AttrValue;
use crate::cfmt::Arg;
use crate::chunk_info::{ChunkInfo, compute_chunk_unpack_size, part_channels};
use crate::context::{ChunkTable, Context, ContextMode, Error, Part, dispatch_write};
use crate::errors::ErrorCode;
use crate::openexr_attr::{Box2i, LineOrder, PixelType, Storage, TileLevelMode};

/// Whether scanline chunk `cidx` may follow chunk `last` (-1 before the part's first). Upstream
/// asks for increasing chunk indices whatever the line order, so a `DECREASING_Y` file stores
/// its chunks from the top, against its line order and unlike the C++ library's; the port
/// asks for them from the bottom, a deviation (docs/improvements.md, I-18).
fn next_scan_chunk(part: &Part, last: i32, cidx: i32) -> bool {
    if part.lineorder == LineOrder::RANDOM_Y {
        true
    } else if part.lineorder == LineOrder::DECREASING_Y {
        cidx == if last == -1 {
            part.chunk_count - 1
        } else {
            last - 1
        }
    } else {
        last == cidx - 1
    }
}

/// The tile levels of `part` in chunk order: each level's first chunk index and its tile
/// counts. `None` without a tile description (which the chunk index computation refuses
/// first).
fn tile_levels_in_chunk_order(part: &Part) -> Option<Vec<(i64, i32, i32)>> {
    let Some(AttrValue::TileDesc(td)) = part.attr(part.tiles).map(|a| a.value()) else {
        return None;
    };
    let (cx, cy) = (&part.tile_level_tile_count_x, &part.tile_level_tile_count_y);
    let (nlx, nly) = (
        part.num_tile_levels_x.max(0) as usize,
        part.num_tile_levels_y.max(0) as usize,
    );
    let levels: Vec<(usize, usize)> = if td.level_mode() == TileLevelMode::RIPMAP_LEVELS {
        (0..nly)
            .flat_map(|ly| (0..nlx).map(move |lx| (lx, ly)))
            .collect()
    } else {
        (0..nlx).map(|l| (l, l)).collect()
    };
    let mut base = 0i64;
    let mut out = Vec::with_capacity(levels.len());
    for (lx, ly) in levels {
        let (nx, ny) = (*cx.get(lx)?, *cy.get(ly)?);
        out.push((base, nx, ny));
        base += i64::from(nx) * i64::from(ny);
    }
    Some(out)
}

/// Whether tile chunk `cidx` may follow chunk `last` (-1 before the part's first). Upstream
/// asks for increasing chunk indices whatever the line order; for a `DECREASING_Y` part the
/// port asks for the C++ library's order instead (`TiledOutputFile::Data::nextTileCoord`,
/// src/lib/OpenEXR/ImfTiledOutputFile.cpp:281-386 @ v3.5.3): the levels in chunk order, each
/// from its top row of tiles down, each row left to right, starting at row `numYTiles - 1` of
/// level 0 (ImfTiledOutputFile.cpp:995-997), a deviation (docs/improvements.md, I-18).
fn next_tile_chunk(part: &Part, last: i32, cidx: i32) -> bool {
    if part.lineorder == LineOrder::RANDOM_Y {
        return true;
    }
    if part.lineorder != LineOrder::DECREASING_Y {
        return last == cidx - 1;
    }
    let Some(levels) = tile_levels_in_chunk_order(part) else {
        return false;
    };
    let next = if last < 0 {
        levels
            .first()
            .map(|&(base, _, ny)| base + i64::from(ny - 1) * i64::from(levels[0].1))
    } else {
        let last = i64::from(last);
        levels
            .iter()
            .position(|&(base, nx, ny)| last >= base && last < base + i64::from(nx) * i64::from(ny))
            .and_then(|li| {
                let (base, nx, _) = levels[li];
                let (mut ty, mut tx) =
                    ((last - base) / i64::from(nx), (last - base) % i64::from(nx));
                let mut li = li;
                tx += 1;
                if tx >= i64::from(nx) {
                    tx = 0;
                    ty -= 1;
                    if ty < 0 {
                        // the next tile is in the next level
                        li += 1;
                        ty = i64::from(levels.get(li)?.2) - 1;
                    }
                }
                let (base, nx, _) = levels[li];
                Some(base + ty * i64::from(nx) + tx)
            })
    };
    next == Some(i64::from(cidx))
}

/// `%p` of a slice argument: NULL for `None`.
fn ptr(b: Option<&[u8]>) -> Arg<'static> {
    Arg::Ptr(b.map_or(0, |b| b.as_ptr() as usize))
}

impl Context {
    /// Port of `alloc_chunk_table` (src/lib/OpenEXRCore/chunk.c:639-674 @ v3.5.3): the part's
    /// chunk table, zeroed on first use, for writing.
    fn alloc_chunk_table(&mut self, part_index: usize) -> Result<&mut Vec<u64>, Error> {
        let part = &mut self.parts[part_index];
        if part.chunk_table.get().is_none() {
            if part.chunk_count < 0 {
                // upstream's allocation of a negative count's bytes fails
                return Err(self.core.standard_error(ErrorCode::OutOfMemory));
            }
            let _ = part
                .chunk_table
                .set(ChunkTable::Table(vec![0; part.chunk_count as usize]));
        }
        match self.parts[part_index].chunk_table.get_mut() {
            Some(ChunkTable::Table(t)) => Ok(t),
            _ => Err(self.core.standard_error(ErrorCode::OutOfMemory)),
        }
    }

    /// The mode checks the chunk writes start with.
    fn require_writing_data(&self) -> Result<(), Error> {
        if self.mode != ContextMode::WritingData {
            if self.mode == ContextMode::Write {
                return Err(self.standard_error(ErrorCode::HeaderNotWritten));
            }
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        Ok(())
    }

    /// Port of `exr_chunk_default_initialize` (src/lib/OpenEXRCore/chunk.c:704-849 @ v3.5.3):
    /// the chunk holding `box`'s first line (or tile) of level (`levelx`, `levely`), without
    /// any I/O; in a temporary context, the part's chunk count is recomputed first. Tiles of a
    /// zero width or height are refused (docs/improvements.md, U-11).
    pub fn chunk_default_initialize(
        &mut self,
        part_index: i32,
        bx: &Box2i,
        levelx: i32,
        levely: i32,
    ) -> Result<ChunkInfo, Error> {
        if part_index < 0 || part_index as usize >= self.parts.len() {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "Part index (%d) out of range",
                &[part_index.into()],
            ));
        }
        let temporary = self.mode == ContextMode::Temporary;
        let pi = part_index as usize;
        let dw = self.parts[pi].data_window;
        if bx.min.y < dw.min.y || bx.min.y > dw.max.y {
            return Err(Error::bare(ErrorCode::InvalidArgument));
        }

        if temporary {
            let part = &mut self.parts[pi];
            part.chunk_count = crate::parse_header::internal_exr_compute_chunk_offset_size(part);
        }
        let part = &self.parts[pi];
        let mut cinfo = ChunkInfo::default();

        if part.storage_mode == Storage::SCANLINE
            || part.storage_mode == Storage::DEEP_SCANLINE
            || (temporary && part.tiles.is_none())
        {
            let lpc = i32::from(part.lines_per_chunk);
            let mut cidx = bx.min.y.wrapping_sub(dw.min.y);
            if lpc > 1 {
                cidx /= lpc;
            }
            // do we need to invert this when reading decreasing y? it appears not
            let miny = dw.min.y.wrapping_add(cidx.wrapping_mul(lpc));

            if cidx < 0 || cidx >= part.chunk_count {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            }

            cinfo.idx = cidx;
            cinfo.type_ = if part.storage_mode == Storage::LAST_TYPE && temporary {
                Storage::SCANLINE.0 as u8
            } else {
                part.storage_mode.0 as u8
            };
            cinfo.compression = part.comp_type.0 as u8;
            cinfo.start_x = dw.min.x;
            cinfo.start_y = miny;
            cinfo.width = dw.max.x.wrapping_sub(dw.min.x).wrapping_add(1);
            cinfo.height = lpc;
            if miny < dw.min.y {
                cinfo.start_y = dw.min.y;
                cinfo.height = cinfo.height.wrapping_sub(dw.min.y.wrapping_sub(miny));
            } else if i64::from(miny) + i64::from(lpc) > i64::from(dw.max.y) {
                cinfo.height = dw.max.y.wrapping_sub(miny).wrapping_add(1);
            }
            cinfo.level_x = 0;
            cinfo.level_y = 0;
            cinfo.unpacked_size =
                compute_chunk_unpack_size(dw.min.x, miny, cinfo.width, cinfo.height, lpc, part);
        } else if let Some(AttrValue::TileDesc(tiledesc)) = part.attr(part.tiles).map(|a| a.value())
        {
            let mut tilew = tiledesc.x_size as i32;
            let mut tileh = tiledesc.y_size as i32;
            if tilew == 0 || tileh == 0 {
                // a division by zero upstream (docs/improvements.md, U-11)
                return Err(Error::bare(ErrorCode::InvalidArgument));
            }

            let tilex = bx.min.x.wrapping_sub(dw.min.x).wrapping_div(tilew);
            let tiley = bx.min.y.wrapping_sub(dw.min.y).wrapping_div(tileh);

            let cidx =
                self.validate_and_compute_tile_chunk_off(part, tilex, tiley, levelx, levely)?;

            let dend = i64::from(part.tile_level_tile_size_x[levelx as usize]);
            let mut tend = i64::from(tilew) * i64::from(tilex + 1);
            if tend > dend {
                tend -= dend;
                if tend < i64::from(tilew) {
                    tilew -= tend as i32;
                }
            }
            let dend = i64::from(part.tile_level_tile_size_y[levely as usize]);
            let mut tend = i64::from(tileh) * i64::from(tiley + 1);
            if tend > dend {
                tend -= dend;
                if tend < i64::from(tileh) {
                    tileh -= tend as i32;
                }
            }

            cinfo.idx = cidx;
            cinfo.type_ = if part.storage_mode == Storage::LAST_TYPE && temporary {
                Storage::TILED.0 as u8
            } else {
                part.storage_mode.0 as u8
            };
            cinfo.compression = part.comp_type.0 as u8;
            cinfo.start_x = tilex;
            cinfo.start_y = tiley;
            cinfo.height = tileh;
            cinfo.width = tilew;
            if levelx > 255 || levely > 255 {
                return Err(Error::bare(ErrorCode::AttrSizeMismatch));
            }
            cinfo.level_x = levelx as u8;
            cinfo.level_y = levely as u8;

            let texels = (tilew as u64).wrapping_mul(tileh as u64);
            let mut unpacksize: u64 = 0;
            for curc in part_channels(part) {
                let sz: u64 = if curc.pixel_type == PixelType::HALF {
                    2
                } else {
                    4
                };
                unpacksize = unpacksize.wrapping_add(texels.wrapping_mul(sz));
            }
            cinfo.unpacked_size = unpacksize;
        }
        Ok(cinfo)
    }

    /// Port of `exr_write_scanline_chunk_info` (src/lib/OpenEXRCore/chunk.c:1870-1958 @ v3.5.3):
    /// the chunk holding scanline `y`, to write. Upstream computes the unpacked size from `y`
    /// rather than the chunk's first line, and its end of the window in `int`; the port uses
    /// the first line and 64 bits, as the read side does, a deviation (docs/improvements.md,
    /// I-17).
    pub fn write_scanline_chunk_info(&self, part_index: i32, y: i32) -> Result<ChunkInfo, Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::TILED || part.storage_mode == Storage::DEEP_TILED {
            return Err(self.standard_error(ErrorCode::ScanTileMixedapi));
        }
        self.require_writing_data()?;

        let dw = part.data_window;
        if y < dw.min.y || y > dw.max.y {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid request for scanline %d outside range of data window (%d - %d)",
                &[y.into(), dw.min.y.into(), dw.max.y.into()],
            ));
        }

        let lpc = i32::from(part.lines_per_chunk);
        let mut cidx = y.wrapping_sub(dw.min.y);
        if lpc > 1 {
            cidx /= lpc;
        }
        let miny = cidx.wrapping_mul(lpc).wrapping_add(dw.min.y);

        if cidx < 0 || cidx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid request for scanline %d in chunk %d outside chunk count %d",
                &[y.into(), cidx.into(), part.chunk_count.into()],
            ));
        }

        let mut cinfo = ChunkInfo {
            idx: cidx,
            type_: part.storage_mode.0 as u8,
            compression: part.comp_type.0 as u8,
            start_x: dw.min.x,
            start_y: miny,
            width: dw.max.x.wrapping_sub(dw.min.x).wrapping_add(1),
            height: lpc,
            ..ChunkInfo::default()
        };
        if miny < dw.min.y {
            cinfo.start_y = dw.min.y;
            cinfo.height = cinfo.height.wrapping_sub(dw.min.y.wrapping_sub(miny));
        } else if i64::from(miny) + i64::from(lpc) > i64::from(dw.max.y) {
            cinfo.height = dw.max.y.wrapping_sub(miny).wrapping_add(1);
        }
        cinfo.unpacked_size =
            compute_chunk_unpack_size(dw.min.x, miny, cinfo.width, cinfo.height, lpc, part);
        Ok(cinfo)
    }

    /// Port of `exr_write_tile_chunk_info` (src/lib/OpenEXRCore/chunk.c:1960-2064 @ v3.5.3): a
    /// tile of a level above 0 is clipped to the level, as the read side does
    /// (docs/improvements.md, I-21).
    pub fn write_tile_chunk_info(
        &self,
        part_index: i32,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<ChunkInfo, Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::SCANLINE || part.storage_mode == Storage::DEEP_SCANLINE {
            return Err(self.standard_error(ErrorCode::TileScanMixedapi));
        }
        self.require_writing_data()?;

        let cidx = self.validate_and_compute_tile_chunk_off(part, tilex, tiley, levelx, levely)?;
        let Some(AttrValue::TileDesc(tiledesc)) = part.attr(part.tiles).map(|a| a.value()) else {
            unreachable!("checked by validate_and_compute_tile_chunk_off")
        };

        // Upstream clips the last tiles of a level against the data window, the size of level 0,
        // so a tile of a smaller level keeps the whole tile size; the port clips against the
        // level's size, as exr_read_tile_chunk_info does (chunk.c:1177-1193 @ v3.5.3), a
        // deviation (docs/improvements.md, I-21). Level 0 gives the same sizes both ways.
        let mut tilew = tiledesc.x_size as i32;
        let dend = i64::from(part.tile_level_tile_size_x[levelx as usize]);
        let mut tend = i64::from(tilew) * i64::from(tilex + 1);
        if tend > dend {
            tend -= dend;
            if tend < i64::from(tilew) {
                tilew -= tend as i32;
            }
        }

        let mut tileh = tiledesc.y_size as i32;
        let dend = i64::from(part.tile_level_tile_size_y[levely as usize]);
        let mut tend = i64::from(tileh) * i64::from(tiley + 1);
        if tend > dend {
            tend -= dend;
            if tend < i64::from(tileh) {
                tileh -= tend as i32;
            }
        }

        let mut cinfo = ChunkInfo {
            idx: cidx,
            type_: part.storage_mode.0 as u8,
            compression: part.comp_type.0 as u8,
            start_x: tilex,
            start_y: tiley,
            height: tileh,
            width: tilew,
            ..ChunkInfo::default()
        };
        if levelx > 255 || levely > 255 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Unable to represent tile level %d, %d in chunk structure",
                &[levelx.into(), levely.into()],
            ));
        }
        cinfo.level_x = levelx as u8;
        cinfo.level_y = levely as u8;

        let mut unpacksize: u64 = 0;
        for curc in part_channels(part) {
            let sz: u64 = if curc.pixel_type == PixelType::HALF {
                2
            } else {
                4
            };
            unpacksize =
                unpacksize.wrapping_add((tilew as u64).wrapping_mul(tileh as u64).wrapping_mul(sz));
        }
        cinfo.unpacked_size = unpacksize;
        Ok(cinfo)
    }

    /// The chunk leader, sample table and data writes, and the part's end, that
    /// `write_scan_chunk` and `write_tile_chunk` share (src/lib/OpenEXRCore/chunk.c:1806-1866,
    /// 2226-2286 @ v3.5.3).
    fn write_chunk_body(
        &mut self,
        part_index: usize,
        cidx: i32,
        leader: &[i32],
        deep: Option<(u64, u64, Option<&[u8]>)>,
        packed_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let leader_bytes: Vec<u8> = leader.iter().flat_map(|v| v.to_le_bytes()).collect();
        let offset = self.output_file_offset;
        self.alloc_chunk_table(part_index)?[cidx as usize] = offset;

        let Context {
            core,
            write_fn,
            output_file_offset,
            ..
        } = self;
        dispatch_write(core, write_fn, &leader_bytes, output_file_offset)?;
        if let Some((packed_size, unpacked_size, sample_data)) = deep {
            let sample = sample_data.unwrap_or(&[]);
            let ddata: Vec<u8> = [sample.len() as u64, packed_size, unpacked_size]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect();
            dispatch_write(core, write_fn, &ddata, output_file_offset)?;
            dispatch_write(core, write_fn, sample, output_file_offset)?;
        }
        if let Some(p) = packed_data.filter(|p| !p.is_empty()) {
            dispatch_write(core, write_fn, p, output_file_offset)?;
        }

        self.output_chunk_count += 1;
        let part: &Part = &self.parts[part_index];
        if self.output_chunk_count == part.chunk_count {
            let mut chunkoff = part.chunk_table_offset;
            self.cur_output_part += 1;
            if self.cur_output_part as usize == self.parts.len() {
                self.core.mode = ContextMode::WriteFinished;
            }
            self.last_output_chunk = -1;
            self.output_chunk_count = 0;

            let table: Vec<u8> = match self.parts[part_index].chunk_table.get() {
                Some(ChunkTable::Table(t)) => t.iter().flat_map(|v| v.to_le_bytes()).collect(),
                _ => Vec::new(),
            };
            dispatch_write(&self.core, &self.write_fn, &table, &mut chunkoff)?;
        } else {
            self.last_output_chunk = cidx;
        }
        Ok(())
    }

    /// Port of `write_scan_chunk` (src/lib/OpenEXRCore/chunk.c:1676-1868 @ v3.5.3).
    fn write_scan_chunk(
        &mut self,
        part_index: i32,
        y: i32,
        packed_data: Option<&[u8]>,
        unpacked_size: u64,
        sample_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        self.require_writing_data()?;
        let pi = part_index as usize;
        let part = &self.parts[pi];
        if part.storage_mode == Storage::TILED || part.storage_mode == Storage::DEEP_TILED {
            return Err(self.standard_error(ErrorCode::ScanTileMixedapi));
        }
        if self.cur_output_part != part_index {
            return Err(self.standard_error(ErrorCode::IncorrectPart));
        }
        let packed_size = packed_data.map_or(0, |p| p.len() as u64);
        let deep = part.storage_mode == Storage::DEEP_SCANLINE;

        if !deep && packed_size > i32::MAX as u64 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Packed data size %llu too large (max %llu)",
                &[Arg::U64(packed_size), Arg::U64(i32::MAX as u64)],
            ));
        }
        let psize = packed_size as i32;

        let sample_size = sample_data.map_or(0, |s| s.len() as u64);
        if deep && sample_size == 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid sample count data argument size %llu pointer %p",
                &[Arg::U64(sample_size), ptr(sample_data)],
            ));
        }

        let dw = part.data_window;
        if y < dw.min.y || y > dw.max.y {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid attempt to write scanlines starting at %d outside range of data window (%d - %d)",
                &[y.into(), dw.min.y.into(), dw.max.y.into()],
            ));
        }

        let lpc = i32::from(part.lines_per_chunk);
        let mut cidx = y.wrapping_sub(dw.min.y);
        if lpc > 1 {
            cidx /= lpc;
        }
        let miny = cidx.wrapping_mul(lpc).wrapping_add(dw.min.y);

        if y != miny {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Attempt to write scanline %d which does not align with y dims (%d) for chunk index (%d)",
                &[y.into(), miny.into(), cidx.into()],
            ));
        }
        if cidx < 0 || cidx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Chunk index for scanline %d in chunk %d outside chunk count %d",
                &[y.into(), cidx.into(), part.chunk_count.into()],
            ));
        }
        if !next_scan_chunk(part, self.last_output_chunk, cidx) {
            return Err(self.standard_error(ErrorCode::IncorrectChunk));
        }

        let mut leader = Vec::with_capacity(3);
        if self.is_multipart {
            leader.push(part_index);
        }
        leader.push(miny);
        if !deep {
            leader.push(psize);
        }
        let deep = deep.then_some((packed_size, unpacked_size, sample_data));
        self.write_chunk_body(pi, cidx, &leader, deep, packed_data)
    }

    /// Port of `exr_write_scanline_chunk` (src/lib/OpenEXRCore/chunk.c:2067-2086 @ v3.5.3):
    /// writes the packed (compressed) chunk starting at scanline `y`.
    pub fn write_scanline_chunk(
        &mut self,
        part_index: i32,
        y: i32,
        packed_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::DEEP_SCANLINE {
            return Err(self.standard_error(ErrorCode::UseScanDeepWrite));
        }
        self.write_scan_chunk(part_index, y, packed_data, 0, None)
    }

    /// Port of `exr_write_deep_scanline_chunk` (src/lib/OpenEXRCore/chunk.c:2089-2121 @ v3.5.3).
    pub fn write_deep_scanline_chunk(
        &mut self,
        part_index: i32,
        y: i32,
        packed_data: Option<&[u8]>,
        unpacked_size: u64,
        sample_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::SCANLINE {
            return Err(self.standard_error(ErrorCode::UseScanNondeepWrite));
        }
        self.write_scan_chunk(part_index, y, packed_data, unpacked_size, sample_data)
    }

    /// Port of `write_tile_chunk` (src/lib/OpenEXRCore/chunk.c:2124-2288 @ v3.5.3).
    #[allow(clippy::too_many_arguments)]
    fn write_tile_chunk_private(
        &mut self,
        part_index: i32,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
        packed_data: Option<&[u8]>,
        unpacked_size: u64,
        sample_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        self.require_writing_data()?;
        let pi = part_index as usize;
        let part = &self.parts[pi];
        if part.storage_mode == Storage::SCANLINE || part.storage_mode == Storage::DEEP_SCANLINE {
            return Err(self.standard_error(ErrorCode::TileScanMixedapi));
        }
        if self.cur_output_part != part_index {
            return Err(self.standard_error(ErrorCode::IncorrectPart));
        }
        let packed_size = packed_data.map_or(0, |p| p.len() as u64);
        let deep = part.storage_mode == Storage::DEEP_TILED;
        // a deep tile whose pixels hold no samples has no packed data, which the scanline
        // writer takes and this one refuses upstream (`!packed_data || packed_size == 0`):
        // deviation D-22 (docs/improvements.md, I-35) takes it here too
        if packed_size == 0 && !deep {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid packed data argument size %llu pointer %p",
                &[Arg::U64(packed_size), ptr(packed_data)],
            ));
        }
        if !deep && packed_size > i32::MAX as u64 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Packed data size %llu too large (max %llu)",
                &[Arg::U64(packed_size), Arg::U64(i32::MAX as u64)],
            ));
        }
        let psize = packed_size as i32;

        let sample_size = sample_data.map_or(0, |s| s.len() as u64);
        if deep && sample_size == 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid sample count data argument size %llu pointer %p",
                &[Arg::U64(sample_size), ptr(sample_data)],
            ));
        }

        let cidx = self.validate_and_compute_tile_chunk_off(part, tilex, tiley, levelx, levely)?;
        if cidx < 0 || cidx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Chunk index for tile (%d, %d) at level (%d, %d) %d outside chunk count %d",
                &[
                    tilex.into(),
                    tiley.into(),
                    levelx.into(),
                    levely.into(),
                    cidx.into(),
                    part.chunk_count.into(),
                ],
            ));
        }
        if !next_tile_chunk(part, self.last_output_chunk, cidx) {
            return Err(self.print_error(
                ErrorCode::IncorrectChunk,
                "Chunk index %d is not the next chunk to be written (last %d)",
                &[cidx.into(), self.last_output_chunk.into()],
            ));
        }

        let mut leader = Vec::with_capacity(6);
        if self.is_multipart {
            leader.push(part_index);
        }
        leader.extend_from_slice(&[tilex, tiley, levelx, levely]);
        if !deep {
            leader.push(psize);
        }
        let deep = deep.then_some((packed_size, unpacked_size, sample_data));
        self.write_chunk_body(pi, cidx, &leader, deep, packed_data)
    }

    /// Port of `exr_write_tile_chunk` (src/lib/OpenEXRCore/chunk.c:2290-2323 @ v3.5.3).
    pub fn write_tile_chunk(
        &mut self,
        part_index: i32,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
        packed_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::DEEP_TILED {
            return Err(self.standard_error(ErrorCode::UseTileDeepWrite));
        }
        self.write_tile_chunk_private(
            part_index,
            tilex,
            tiley,
            levelx,
            levely,
            packed_data,
            0,
            None,
        )
    }

    /// Port of `exr_write_deep_tile_chunk` (src/lib/OpenEXRCore/chunk.c:2326-2363 @ v3.5.3).
    #[allow(clippy::too_many_arguments)]
    pub fn write_deep_tile_chunk(
        &mut self,
        part_index: i32,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
        packed_data: Option<&[u8]>,
        unpacked_size: u64,
        sample_data: Option<&[u8]>,
    ) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::TILED {
            return Err(self.standard_error(ErrorCode::UseTileNondeepWrite));
        }
        self.write_tile_chunk_private(
            part_index,
            tilex,
            tiley,
            levelx,
            levely,
            packed_data,
            unpacked_size,
            sample_data,
        )
    }

    // used by the encode pipeline (encoding.c), the next chunk of Phase 6
    /// Port of `internal_validate_next_chunk` (src/lib/OpenEXRCore/chunk.c:2365-2428 @ v3.5.3):
    /// whether the encode pipeline's chunk is the next one this context can write.
    pub(crate) fn internal_validate_next_chunk(
        &self,
        part_index: i32,
        chunk: &ChunkInfo,
    ) -> Result<(), Error> {
        if self.cur_output_part != part_index {
            return Err(self.standard_error(ErrorCode::IncorrectPart));
        }
        let part = &self.parts[part_index as usize];
        let cidx =
            if part.storage_mode == Storage::TILED || part.storage_mode == Storage::DEEP_TILED {
                self.validate_and_compute_tile_chunk_off(
                    part,
                    chunk.start_x,
                    chunk.start_y,
                    i32::from(chunk.level_x),
                    i32::from(chunk.level_y),
                )?
            } else {
                let lpc = i32::from(part.lines_per_chunk);
                let mut cidx = chunk.start_y.wrapping_sub(part.data_window.min.y);
                if lpc > 1 {
                    cidx /= lpc;
                }
                cidx
            };

        let tiled = part.storage_mode == Storage::TILED || part.storage_mode == Storage::DEEP_TILED;
        if cidx < 0 || cidx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Chunk index for scanline %d in chunk %d outside chunk count %d",
                &[chunk.start_y.into(), cidx.into(), part.chunk_count.into()],
            ));
        }
        let next = if tiled {
            next_tile_chunk(part, self.last_output_chunk, cidx)
        } else {
            next_scan_chunk(part, self.last_output_chunk, cidx)
        };
        if !next {
            return Err(self.print_error(
                ErrorCode::IncorrectChunk,
                "Attempt to write chunk %d, but last output chunk is %d",
                &[cidx.into(), self.last_output_chunk.into()],
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::chunk_info::ChunkInfo;
    use crate::context::{Context, Error};
    use crate::errors::ErrorCode;
    use crate::openexr_attr::{Storage, TileLevelMode};
    use crate::upstream_tests::read::test_image_dir;
    use crate::write_header::tests::copy_header;

    /// A chunk of a file: its first line (or tile x, tile y and levels) and its information.
    pub(crate) type FileChunk = (i32, i32, i32, i32, ChunkInfo);

    /// Part `p`'s chunks of `src`, in the order the C++ library writes them: chunk order; for a
    /// decreasing-Y part, from the bottom (scanlines), or each level from its top row of tiles
    /// down (tiles).
    pub(crate) fn chunks_in_write_order(src: &Context, p: i32) -> Result<Vec<FileChunk>, Error> {
        let mut chunks = Vec::new();
        if src.get_storage(p)? == Storage::SCANLINE {
            let dw = src.get_data_window(p)?;
            let lpc = src.get_scanlines_per_chunk(p)?;
            let mut y = dw.min.y;
            while y <= dw.max.y {
                chunks.push((y, 0, 0, 0, src.read_scanline_chunk_info(p, y)?));
                y += lpc;
            }
        } else {
            let (nlx, nly) = src.get_tile_levels(p)?;
            let mode = src.get_tile_descriptor(p)?.2;
            let levels: Vec<(i32, i32)> = if mode == TileLevelMode::RIPMAP_LEVELS {
                (0..nly)
                    .flat_map(|ly| (0..nlx).map(move |lx| (lx, ly)))
                    .collect()
            } else {
                (0..nlx).map(|l| (l, l)).collect()
            };
            for (lx, ly) in levels {
                let (cx, cy) = src.get_tile_counts(p, lx, ly)?;
                for ty in 0..cy {
                    for tx in 0..cx {
                        chunks.push((tx, ty, lx, ly, src.read_tile_chunk_info(p, tx, ty, lx, ly)?));
                    }
                }
            }
        }
        chunks.sort_by_key(|c| c.4.idx);
        if src.get_storage(p)? == Storage::SCANLINE
            && src.get_lineorder(p)? == crate::openexr_attr::LineOrder::DECREASING_Y
        {
            chunks.reverse();
        } else if src.get_lineorder(p)? == crate::openexr_attr::LineOrder::DECREASING_Y {
            // tiles: each level from its top row down, each row left to right
            let mut first = std::collections::HashMap::new();
            for c in &chunks {
                let e = first.entry((c.2, c.3)).or_insert(c.4.idx);
                *e = (*e).min(c.4.idx);
            }
            chunks.sort_by_key(|c| (first[&(c.2, c.3)], std::cmp::Reverse(c.1), c.0));
        }
        Ok(chunks)
    }

    /// The whole file, rewritten: the header copied (legacy order), then every chunk's packed
    /// bytes read from `src` and written in chunk order, with the chunk tables.
    fn rewrite_file(src: &Context) -> Result<Vec<u8>, Error> {
        let (mut dst, mem) = copy_header(src)?;
        for p in 0..src.get_count() {
            for (a, b, lx, ly, cinfo) in chunks_in_write_order(src, p)? {
                let mut packed = vec![0u8; cinfo.packed_size as usize];
                src.read_chunk(p, &cinfo, Some(&mut packed))?;
                if src.get_storage(p)? == Storage::SCANLINE {
                    dst.write_scanline_chunk(p, a, Some(&packed))?;
                } else {
                    dst.write_tile_chunk(p, a, b, lx, ly, Some(&packed))?;
                }
            }
        }
        let bytes = mem.lock().unwrap().clone();
        Ok(bytes)
    }

    /// The scanline and tiled images the C++ library wrote come back byte for byte, chunk
    /// tables and chunk leaders included; a decreasing-Y image's chunks are written from the
    /// bottom, as its line order says (docs/improvements.md, I-18).
    #[test]
    fn files_rewrite_as_the_cpp_library_wrote_them() {
        let quiet = || {
            Some(crate::context::Initializer {
                error_handler: Some(std::sync::Arc::new(|_| {})),
                ..crate::context::Initializer::default()
            })
        };
        let mut same = 0;
        for n in [
            "comp_none.exr",
            "comp_rle.exr",
            "comp_zip.exr",
            "comp_zips.exr",
            "comp_piz.exr",
            "comp_b44.exr",
            "comp_dwaa_v2.exr",
            "lineOrder_increasing.exr",
            "lineOrder_decreasing.exr",
            "v1.7.test.1.exr",
            "v1.7.test.interleaved.exr",
            "v1.7.test.planar.exr",
            "v1.7.test.tiled.exr",
            "test_native1.exr",
        ] {
            let path = test_image_dir() + n;
            let src = Context::start_read(Some(&path), quiet()).expect(n);
            let file = std::fs::read(&path).unwrap();
            let out = rewrite_file(&src).unwrap_or_else(|e| panic!("{n}: {e}"));
            assert_eq!(out.len(), file.len(), "{n}: size");
            assert!(out == file, "{n}: bytes differ");
            same += 1;
        }
        assert!(same > 0);
    }

    /// A mipmapped `DECREASING_Y` tiled part takes its tiles in the C++ library's order (each
    /// level from its top row down, each row left to right) and refuses chunk order; every
    /// tile reads back from where it was written (docs/improvements.md, I-18). The wheel
    /// writes only level 0 of such a part, so the order across levels rests on the C++
    /// library's source (ImfTiledOutputFile.cpp:281-386 @ v3.5.3).
    #[test]
    fn decreasing_mipmapped_tiles_go_in_the_cpp_librarys_order() {
        use crate::context::DefaultWriteMode;
        use crate::openexr_attr::TileRoundMode;
        use crate::openexr_attr::{Compression, LineOrder, PerceptualTreatment, PixelType};

        let path = crate::upstream_tests::read::make_temp_dir() + "decreasing_mip.exr";
        let mut f =
            Context::start_write(Some(&path), DefaultWriteMode::WriteFileDirectly, None).unwrap();
        let p = f.add_part(Some("mip"), Storage::TILED).unwrap();
        f.initialize_required_attr_simple(p, 37, 23, Compression::NONE)
            .unwrap();
        f.add_channel(
            p,
            Some("Y"),
            PixelType::FLOAT,
            PerceptualTreatment::LINEAR,
            1,
            1,
        )
        .unwrap();
        f.set_tile_descriptor(
            p,
            16,
            8,
            TileLevelMode::MIPMAP_LEVELS,
            TileRoundMode::ROUND_DOWN,
        )
        .unwrap();
        f.set_lineorder(p, LineOrder::DECREASING_Y).unwrap();
        f.write_header().unwrap();

        let (nlx, _) = f.get_tile_levels(p).unwrap();
        assert!(nlx > 1, "a mipmapped part");
        let mut order = Vec::new();
        for l in 0..nlx {
            let (cx, cy) = f.get_tile_counts(p, l, l).unwrap();
            for ty in (0..cy).rev() {
                for tx in 0..cx {
                    order.push((tx, ty, l));
                }
            }
        }
        // the chunk order is refused: the first tile is the top-left one of level 0's last row
        let (_, cy0) = f.get_tile_counts(p, 0, 0).unwrap();
        assert!(cy0 > 1);
        let first = f.write_tile_chunk_info(p, 0, 0, 0, 0).unwrap();
        let data = vec![0u8; first.unpacked_size as usize];
        assert_eq!(
            f.write_tile_chunk(p, 0, 0, 0, 0, Some(&data))
                .unwrap_err()
                .code,
            ErrorCode::IncorrectChunk
        );

        // each tile's bytes: its position in the order, repeated
        let bytes = |i: usize, n: usize| vec![i as u8; n];
        for (i, &(tx, ty, l)) in order.iter().enumerate() {
            let c = f.write_tile_chunk_info(p, tx, ty, l, l).unwrap();
            let data = bytes(i, c.unpacked_size as usize);
            f.write_tile_chunk(p, tx, ty, l, l, Some(&data)).unwrap();
        }
        f.finish().unwrap();

        let r = Context::start_read(Some(&path), None).unwrap();
        for (i, &(tx, ty, l)) in order.iter().enumerate() {
            let c = r.read_tile_chunk_info(p, tx, ty, l, l).unwrap();
            let mut got = vec![0u8; c.packed_size as usize];
            r.read_chunk(p, &c, Some(&mut got)).unwrap();
            assert!(got == bytes(i, got.len()), "tile {tx}, {ty} of level {l}");
        }
        let _ = std::fs::remove_file(&path);
    }
}
