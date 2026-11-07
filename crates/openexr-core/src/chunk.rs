// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `chunk.c` (src/lib/OpenEXRCore/chunk.c:1-2428 @ v3.5.3), so far the chunk table:
//! reading it, validating it, and rebuilding it from the chunk leaders when a file's table is
//! incomplete. The chunk reads follow; the chunk writes come with Phase 6.

use crate::attributes::AttrValue;
use crate::cfmt::Arg;
use crate::channel_list::AttrChlistEntry;
use crate::context::{ChunkTable, Context, ContextMode, Error, Part, ReadMode, dispatch_read};
use crate::errors::ErrorCode;
use crate::internal_util::{compute_sampled_height, compute_sampled_width};
use crate::internal_xdr::one_to_native64;
use crate::openexr_attr::{Compression, LineOrder, PixelType, Storage, TileLevelMode};

/// Port of `struct priv_chunk_leader` (src/lib/OpenEXRCore/chunk.c:192-208 @ v3.5.3).
/// `scanline_y` shares its storage with `tile_x` upstream.
#[derive(Debug, Default, Clone, Copy)]
struct ChunkLeader {
    partnum: i32,
    scanline_y: i32,
    tile_x: i32,
    tile_y: i32,
    level_x: i32,
    level_y: i32,
    packed_size: u64,
}

impl Context {
    /// `do_read` with `EXR_MUST_READ_ALL`.
    pub(crate) fn read_all(&self, buf: &mut [u8], offset: &mut u64) -> Result<(), Error> {
        let mut nread = 0;
        dispatch_read(
            &self.core,
            &self.read_fn,
            buf,
            offset,
            &mut nread,
            ReadMode::MustReadAll,
        )
    }

    /// Port of `validate_and_compute_tile_chunk_off` (src/lib/OpenEXRCore/chunk.c:26-188
    /// @ v3.5.3): the chunk index of a tile.
    pub(crate) fn validate_and_compute_tile_chunk_off(
        &self,
        part: &Part,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<i32, Error> {
        let tiledesc = match part.attr(part.tiles).map(|a| a.value()) {
            Some(AttrValue::TileDesc(t))
                if part.num_tile_levels_x > 0
                    && part.num_tile_levels_y > 0
                    && !part.tile_level_tile_count_x.is_empty()
                    && !part.tile_level_tile_count_y.is_empty() =>
            {
                *t
            }
            _ => {
                return Err(self.print_error(
                    ErrorCode::MissingReqAttr,
                    "Tile descriptor data missing or corrupt",
                    &[],
                ));
            }
        };

        if tilex < 0 || tiley < 0 || levelx < 0 || levely < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid tile indices provided (%d, %d, level %d, %d)",
                &[tilex.into(), tiley.into(), levelx.into(), levely.into()],
            ));
        }

        let cx = &part.tile_level_tile_count_x;
        let cy = &part.tile_level_tile_count_y;
        let mut chunkoff: i64 = 0;
        let lm = tiledesc.level_mode();
        if lm == TileLevelMode::ONE_LEVEL || lm == TileLevelMode::MIPMAP_LEVELS {
            if levelx != levely {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) level (%d, %d), but single level and mipmap tiles must have same level x and y",
                    &[tilex.into(), tiley.into(), levelx.into(), levely.into()],
                ));
            }
            if levelx >= part.num_tile_levels_x {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) level %d, but level past available levels (%d)",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        part.num_tile_levels_x.into(),
                    ],
                ));
            }
            let numx = cx[levelx as usize];
            let numy = cy[levelx as usize];
            if tilex >= numx || tiley >= numy {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) level %d, but level only has %d x %d tiles",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        numx.into(),
                        numy.into(),
                    ],
                ));
            }
            for l in 0..levelx as usize {
                chunkoff += i64::from(cx[l]) * i64::from(cy[l]);
            }
            // int arithmetic, which wraps for levels of more than 2^31 tiles
            chunkoff += i64::from(tiley.wrapping_mul(numx).wrapping_add(tilex));
        } else if lm == TileLevelMode::RIPMAP_LEVELS {
            if levelx >= part.num_tile_levels_x {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) level %d, %d, but x level past available levels (%d)",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        levely.into(),
                        part.num_tile_levels_x.into(),
                    ],
                ));
            }
            if levely >= part.num_tile_levels_y {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) level %d, %d, but y level past available levels (%d)",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        levely.into(),
                        part.num_tile_levels_y.into(),
                    ],
                ));
            }
            let numx = cx[levelx as usize];
            let numy = cy[levely as usize];
            if tilex >= numx || tiley >= numy {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Request for tile (%d, %d) at rip level %d, %d level only has %d x %d tiles",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        levely.into(),
                        numx.into(),
                        numy.into(),
                    ],
                ));
            }
            for ly in 0..levely as usize {
                for lx in 0..part.num_tile_levels_x as usize {
                    chunkoff += i64::from(cx[lx]) * i64::from(cy[ly]);
                }
            }
            for lx in 0..levelx as usize {
                chunkoff += i64::from(cx[lx]) * i64::from(numy);
            }
            chunkoff += i64::from(tiley.wrapping_mul(numx).wrapping_add(tilex));
        } else {
            return Err(self.print_error(ErrorCode::Unknown, "Invalid tile description", &[]));
        }

        // a negative offset (the int sum wrapped) is refused too (docs/improvements.md, U-7)
        if chunkoff >= i64::from(part.chunk_count) || chunkoff < 0 {
            return Err(self.print_error(
                ErrorCode::Unknown,
                "Invalid tile chunk offset %lld (%d avail)",
                &[Arg::I64(chunkoff), part.chunk_count.into()],
            ));
        }
        Ok(chunkoff as i32)
    }

    /// Port of `extract_chunk_leader` (src/lib/OpenEXRCore/chunk.c:212-342 @ v3.5.3): reads
    /// the leader of the chunk at `offset`; returns it and the offset past the chunk.
    fn extract_chunk_leader(
        &self,
        part: &Part,
        partnum: i32,
        offset: u64,
    ) -> Result<(u64, ChunkLeader), Error> {
        let mut leader = ChunkLeader::default();
        let mut nextoffset = offset;
        let mut maxval = i64::from(i32::MAX); // 2GB
        if self.file_size > 0 {
            maxval = self.file_size;
        }

        let sm = part.storage_mode;
        let ntoread: usize = if sm == Storage::SCANLINE || sm == Storage::DEEP_SCANLINE {
            let n = if self.is_multipart { 2 } else { 1 };
            if sm != Storage::DEEP_SCANLINE {
                n + 1
            } else {
                n
            }
        } else if sm == Storage::DEEP_TILED {
            if self.is_multipart { 5 } else { 4 }
        } else if self.is_multipart {
            6
        } else {
            5
        };

        let mut raw = [0u8; 24];
        self.read_all(&mut raw[..ntoread * 4], &mut nextoffset)?;
        let mut data = [0i32; 6];
        for (d, b) in data.iter_mut().zip(raw.as_chunks::<4>().0) {
            *d = i32::from_le_bytes(*b);
        }

        let mut rdcnt = 0;
        if self.is_multipart {
            if data[rdcnt] != partnum {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid part number reconstructing chunk table: expect %d, found %d",
                    &[partnum.into(), data[rdcnt].into()],
                ));
            }
            leader.partnum = partnum;
            rdcnt += 1;
        } else {
            leader.partnum = 0;
        }

        if sm == Storage::SCANLINE || sm == Storage::DEEP_SCANLINE {
            leader.scanline_y = data[rdcnt];
        } else {
            leader.tile_x = data[rdcnt];
            leader.tile_y = data[rdcnt + 1];
            leader.level_x = data[rdcnt + 2];
            leader.level_y = data[rdcnt + 3];
            rdcnt += 3;
            leader.scanline_y = leader.tile_x;
        }

        if sm == Storage::DEEP_SCANLINE || sm == Storage::DEEP_TILED {
            let mut raw = [0u8; 24];
            self.read_all(&mut raw, &mut nextoffset)?;
            let mut deep_data = [0i64; 3];
            for (d, b) in deep_data.iter_mut().zip(raw.as_chunks::<8>().0) {
                *d = i64::from_le_bytes(*b);
            }
            if deep_data[0] < 0 || (deep_data[0] == 0 && (deep_data[1] != 0 || deep_data[2] != 0)) {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid chunk size reconstructing chunk table: found out of range sample count %lld",
                    &[Arg::I64(deep_data[0])],
                ));
            }
            if deep_data[1] < 0 || deep_data[1] > maxval || (deep_data[1] == 0 && deep_data[2] != 0)
            {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid chunk size reconstructing chunk table: found out of range %lld",
                    &[Arg::I64(deep_data[1])],
                ));
            }
            leader.packed_size = (deep_data[0] as u64).wrapping_add(deep_data[1] as u64);
        } else {
            rdcnt += 1;
            if data[rdcnt] < 0 || i64::from(data[rdcnt]) > maxval {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid chunk size reconstructing chunk table: found out of range %d",
                    &[data[rdcnt].into()],
                ));
            }
            leader.packed_size = data[rdcnt] as u64;
        }
        nextoffset = nextoffset.wrapping_add(leader.packed_size);
        Ok((nextoffset, leader))
    }

    /// Port of `read_and_validate_chunk_leader` (src/lib/OpenEXRCore/chunk.c:360-416 @ v3.5.3):
    /// reads the leader at `offset` and checks its chunk index against `*indexio`, which it
    /// sets to the index the leader names. `*next_offset` moves past the chunk once its leader
    /// was read, even when the index check fails.
    fn read_and_validate_chunk_leader(
        &self,
        part: &Part,
        partnum: i32,
        offset: u64,
        indexio: &mut i32,
        next_offset: &mut u64,
    ) -> Result<(), Error> {
        let (next, leader) = self.extract_chunk_leader(part, partnum, offset)?;
        *next_offset = next;

        let sm = part.storage_mode;
        if sm == Storage::SCANLINE || sm == Storage::DEEP_SCANLINE {
            let mut chunk = i64::from(leader.scanline_y);
            chunk -= i64::from(part.data_window.min.y);
            chunk /= i64::from(part.lines_per_chunk);

            // scanlines can be more strict about the ordering
            let rv = if *indexio != chunk as i32
                || chunk < 0
                || chunk >= i64::from(part.chunk_count)
            {
                Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid chunk index: %lld reading scanline %d (datawindow min %d) with lines per chunk %d",
                    &[
                        Arg::I64(chunk),
                        leader.scanline_y.into(),
                        part.data_window.min.y.into(),
                        i32::from(part.lines_per_chunk).into(),
                    ],
                ))
            } else {
                Ok(())
            };
            *indexio = chunk as i32;
            rv
        } else {
            // because of random order, just go with it if the coordinates look sane
            let rv = self.validate_and_compute_tile_chunk_off(
                part,
                leader.tile_x,
                leader.tile_y,
                leader.level_x,
                leader.level_y,
            );
            *indexio = *rv.as_ref().unwrap_or(&0);
            rv.map(|_| ())
        }
    }

    /// Port of `reconstruct_chunk_table` (src/lib/OpenEXRCore/chunk.c:418-510 @ v3.5.3): walks
    /// the chunks from the end of the previous part (or of the chunk tables), filling in
    /// `chunktable` from the leaders found. "This should behave the same as the old
    /// ImfMultiPartInputFile."
    fn reconstruct_chunk_table(&self, partnum: usize, chunktable: &mut [u64]) -> Result<(), Error> {
        let part = &self.parts[partnum];
        let mut firstfailrv: Result<(), Error> = Ok(());

        let last = self.parts.last().expect("a part");
        let mut offset_start = last
            .chunk_table_offset
            .wrapping_add(8u64.wrapping_mul(i64::from(last.chunk_count) as u64));

        let mut max_offset = u64::MAX;
        if self.file_size > 0 {
            max_offset = self.file_size as u64;
        }

        // for multi-part, need to start at the first part and extract everything, then work
        // our way back up to this one, then grab the end of the previous part
        if partnum > 0 {
            let prevpart = &self.parts[partnum - 1];
            let (curctable, _) = self.extract_chunk_table(partnum - 1)?;
            let mut chunk_start = offset_start;
            for &c in curctable.iter().take(prevpart.chunk_count.max(0) as usize) {
                if c > chunk_start && c < max_offset {
                    chunk_start = c;
                }
            }
            // extract_chunk_size
            let (next, _) = self.extract_chunk_leader(prevpart, partnum as i32 - 1, chunk_start)?;
            offset_start = next;
        }

        let count = part.chunk_count.max(0) as usize;
        let mut curctable = vec![0u64; count];
        for ci in 0..count {
            if chunktable[ci] >= offset_start && chunktable[ci] < max_offset {
                offset_start = chunktable[ci];
            }
            let mut chunk_start = offset_start;
            let mut computed_ci = ci as i32;
            if part.lineorder == LineOrder::DECREASING_Y {
                computed_ci = part.chunk_count - (ci as i32 + 1);
            }
            let mut found_ci = computed_ci;

            let rv = self.read_and_validate_chunk_leader(
                part,
                partnum as i32,
                chunk_start,
                &mut found_ci,
                &mut offset_start,
            );
            if let Err(e) = rv {
                chunk_start = 0;
                if firstfailrv.is_ok() {
                    firstfailrv = Err(e);
                }
            }

            if found_ci >= 0 && found_ci < part.chunk_count && curctable[found_ci as usize] == 0 {
                curctable[found_ci as usize] = chunk_start;
            }
        }
        if firstfailrv.is_ok() {
            chunktable[..count].copy_from_slice(&curctable);
        } else {
            for (dst, &c) in chunktable.iter_mut().zip(&curctable) {
                if c != 0 {
                    *dst = c;
                }
            }
        }
        firstfailrv
    }

    /// Port of `extract_chunk_table` (src/lib/OpenEXRCore/chunk.c:526-637 @ v3.5.3): the
    /// part's chunk table, read on first use (and rebuilt when incomplete, unless the context
    /// disables that), and the smallest offset a chunk can have.
    pub(crate) fn extract_chunk_table(&self, part_index: usize) -> Result<(&[u64], u64), Error> {
        let part = &self.parts[part_index];
        let mut chunkoff = part.chunk_table_offset;
        let chunkbytes = 8u64.wrapping_mul(i64::from(part.chunk_count) as u64);
        let chunkminoffset = chunkoff.wrapping_add(chunkbytes);

        if part.chunk_table.get().is_none() {
            if part.chunk_count <= 0 {
                return Err(
                    self.report_error(ErrorCode::InvalidArgument, b"Invalid file with no chunks")
                );
            }

            // some of the stream-based objects can't reliably check the file size so the C++
            // layer also had an arbitrary stop at 2^20 chunk entries which seems safe...
            if part.chunk_count > 1024 * 1024
                || (self.file_size > 0 && chunkbytes.wrapping_add(chunkoff) > self.file_size as u64)
            {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "chunk table size (%llu) too big for file size (%lld)",
                    &[Arg::U64(chunkbytes), Arg::I64(self.file_size)],
                ));
            }

            let mut raw = vec![0u8; chunkbytes as usize];
            let state = if self.read_all(&mut raw, &mut chunkoff).is_err() {
                ChunkTable::Failed
            } else {
                let mut ctable: Vec<u64> = raw
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|b| u64::from_ne_bytes(*b))
                    .collect();
                let mut failed = false;
                if !self.disable_chunk_reconstruct {
                    // could convert table all at once, but need to check if the file is
                    // incomplete (i.e. crashed during write and didn't get a complete chunk
                    // table), so just do them one at a time
                    let mut maxoff = u64::MAX;
                    if self.file_size > 0 {
                        maxoff = self.file_size as u64;
                    }
                    let mut complete = true;
                    for c in &mut ctable {
                        let cchunk = one_to_native64(*c);
                        if cchunk < chunkoff || cchunk >= maxoff {
                            complete = false;
                        }
                        *c = cchunk;
                    }

                    if !complete {
                        // The c++ side would basically fail as soon as it failed, but would
                        // otherwise swallow all errors, and then just let the reads fail
                        // later. We will do something similar, except when in strict mode,
                        // we will fail with a corrupt chunk immediately.
                        if self
                            .reconstruct_chunk_table(part_index, &mut ctable)
                            .is_err()
                            && self.strict_header
                        {
                            failed = true;
                            self.report_error(
                                ErrorCode::BadChunkLeader,
                                b"Incomplete / corrupt chunk table, unable to reconstruct",
                            );
                        }
                    }
                } else {
                    for c in &mut ctable {
                        *c = one_to_native64(*c);
                    }
                }
                if failed {
                    ChunkTable::Failed
                } else {
                    ChunkTable::Table(ctable)
                }
            };
            // see if we win or not
            let _ = part.chunk_table.set(state);
        }

        match part.chunk_table.get() {
            Some(ChunkTable::Table(t)) => Ok((t, chunkminoffset)),
            _ => Err(Error::bare(ErrorCode::BadChunkLeader)),
        }
    }

    /// Port of `exr_get_chunk_table_offset` (src/lib/OpenEXRCore/chunk.c:512-524 @ v3.5.3): the
    /// file offset of the part's chunk table.
    pub fn get_chunk_table_offset(&self, part_index: i32) -> Result<u64, Error> {
        Ok(self.part_ref(part_index)?.chunk_table_offset)
    }
}

/// Port of `exr_chunk_info_t` (src/lib/OpenEXRCore/openexr_chunkio.h:31-54 @ v3.5.3): where a
/// chunk is and what it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChunkInfo {
    pub idx: i32,
    /// For tiles, this is the tilex; for scans it is the x.
    pub start_x: i32,
    /// For tiles, this is the tiley; for scans it is the scanline y.
    pub start_y: i32,
    /// For this chunk.
    pub height: i32,
    /// For this chunk.
    pub width: i32,
    /// For tiled files.
    pub level_x: u8,
    /// For tiled files.
    pub level_y: u8,
    pub type_: u8,
    pub compression: u8,
    pub data_offset: u64,
    pub packed_size: u64,
    pub unpacked_size: u64,
    pub sample_count_data_offset: u64,
    pub sample_count_table_size: u64,
}

/// The part's channel list (a part being read has one: header validation requires it).
fn part_channels(part: &Part) -> &[AttrChlistEntry] {
    match part.attr(part.channels).map(|a| a.value()) {
        Some(AttrValue::Chlist(cl)) => &cl.entries[..cl.num_channels.max(0) as usize],
        _ => &[],
    }
}

/// Port of `compute_chunk_unpack_size` (src/lib/OpenEXRCore/chunk.c:676-700 @ v3.5.3).
fn compute_chunk_unpack_size(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    lpc: i32,
    part: &Part,
) -> u64 {
    let mut unpacksize: u64 = 0;
    if part.chan_has_line_sampling != 0 || height != lpc {
        for curc in part_channels(part) {
            let mut chansz: u64 = if curc.pixel_type == PixelType::HALF {
                2
            } else {
                4
            };
            chansz = chansz.wrapping_mul(compute_sampled_width(width, curc.x_sampling, x) as u64);
            chansz = chansz.wrapping_mul(compute_sampled_height(height, curc.y_sampling, y) as u64);
            unpacksize = unpacksize.wrapping_add(chansz);
        }
    } else {
        unpacksize = part.unpacked_size_per_chunk;
    }
    unpacksize
}

impl Context {
    /// `EXR_READONLY_AND_DEFINE_PART` (src/lib/OpenEXRCore/internal_structs.h:329-340
    /// @ v3.5.3): part `part_index` of a context open for reading.
    pub(crate) fn part_read_only(&self, part_index: i32) -> Result<&Part, Error> {
        if self.mode != ContextMode::Read {
            return Err(self.standard_error(ErrorCode::NotOpenRead));
        }
        self.part_ref(part_index)
    }

    /// Port of `exr_read_scanline_chunk_info` (src/lib/OpenEXRCore/chunk.c:853-1137
    /// @ v3.5.3): the chunk holding scanline `y`, from the chunk table and the chunk's leader.
    pub fn read_scanline_chunk_info(&self, part_index: i32, y: i32) -> Result<ChunkInfo, Error> {
        let part = self.part_read_only(part_index)?;
        let mut cinfo = ChunkInfo::default();

        if part.storage_mode != Storage::SCANLINE && part.storage_mode != Storage::DEEP_SCANLINE {
            return Err(self.standard_error(ErrorCode::ScanTileMixedapi));
        }

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

        // do we need to invert this when reading decreasing y? it appears not
        let miny = dw.min.y.wrapping_add(cidx.wrapping_mul(lpc));

        if cidx < 0 || cidx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid request for scanline %d in chunk %d outside chunk count %d",
                &[y.into(), cidx.into(), part.chunk_count.into()],
            ));
        }

        cinfo.idx = cidx;
        cinfo.type_ = part.storage_mode.0 as u8;
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

        // need to read from the file to get the packed chunk size
        let (ctable, chunkmin) = self.extract_chunk_table(part_index as usize)?;

        let fsize = self.file_size;
        let mut dataoff = ctable[cidx as usize];

        // known behavior for partial files
        if dataoff == 0 {
            return Err(Error::bare(ErrorCode::IncompleteChunkTable));
        }

        if dataoff < chunkmin || (fsize > 0 && dataoff > fsize as u64) {
            return Err(self.print_error(
                ErrorCode::BadChunkLeader,
                "Corrupt chunk offset table: scanline %d, chunk index %d recorded at file offset %llu",
                &[y.into(), cidx.into(), Arg::U64(dataoff)],
            ));
        }

        // multi part files have the part for validation
        let mut rdcnt: usize = if self.is_multipart { 2 } else { 1 };
        // deep has 64-bit data, so be variable about what we read
        if part.storage_mode != Storage::DEEP_SCANLINE {
            rdcnt += 1;
        }

        let mut raw = [0u8; 12];
        self.read_all(&mut raw[..rdcnt * 4], &mut dataoff)?;
        let mut data = [0i32; 3];
        for (d, b) in data.iter_mut().zip(raw.as_chunks::<4>().0) {
            *d = i32::from_le_bytes(*b);
        }

        let mut rdcnt = 0;
        if self.is_multipart {
            if data[rdcnt] != part_index {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing read scanline %d (chunk %d), found corrupt leader: part says %d, expected %d",
                    &[y.into(), cidx.into(), data[rdcnt].into(), part_index.into()],
                ));
            }
            rdcnt += 1;
        }
        if miny != data[rdcnt] {
            return Err(self.print_error(
                ErrorCode::BadChunkLeader,
                "Preparing to read scanline %d (chunk %d), found corrupt leader: scanline says %d, expected %d",
                &[y.into(), cidx.into(), data[rdcnt].into(), miny.into()],
            ));
        }

        if part.storage_mode == Storage::DEEP_SCANLINE {
            let mut raw = [0u8; 24];
            self.read_all(&mut raw, &mut dataoff)?;
            let mut ddata = [0i64; 3];
            for (d, b) in ddata.iter_mut().zip(raw.as_chunks::<8>().0) {
                *d = i64::from_le_bytes(*b);
            }

            if ddata[0] < 0 || ddata[0] > i64::from(i32::MAX) {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: invalid sample table size %lld",
                    &[y.into(), cidx.into(), Arg::I64(ddata[0])],
                ));
            }
            if ddata[1] < 0 || ddata[1] > i64::from(i32::MAX) {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: invalid packed data size %lld",
                    &[y.into(), cidx.into(), Arg::I64(ddata[1])],
                ));
            }
            if ddata[2] < 0 || ddata[2] > i64::from(i32::MAX) {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to scanline %d (chunk %d), found corrupt leader: unsupported unpacked data size %lld",
                    &[y.into(), cidx.into(), Arg::I64(ddata[2])],
                ));
            }

            cinfo.sample_count_data_offset = dataoff;
            cinfo.sample_count_table_size = ddata[0] as u64;
            cinfo.data_offset = dataoff.wrapping_add(ddata[0] as u64);
            cinfo.packed_size = ddata[1] as u64;
            cinfo.unpacked_size = ddata[2] as u64;

            // uncompressed reads don't have same checks as compressed, so pre-check that the
            // data is of a valid size
            let chunkmin = (cinfo.width as u64)
                .wrapping_mul(cinfo.height as u64)
                .wrapping_mul(4);
            if part.comp_type == Compression::NONE && cinfo.sample_count_table_size != chunkmin {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Invalid deep sample count size, must be one entry per pixel: found %llu expected %llu",
                    &[Arg::U64(cinfo.sample_count_table_size), Arg::U64(chunkmin)],
                ));
            }

            if fsize > 0
                && (cinfo
                    .sample_count_data_offset
                    .wrapping_add(cinfo.sample_count_table_size)
                    > fsize as u64
                    || cinfo.data_offset.wrapping_add(cinfo.packed_size) > fsize as u64)
            {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to scanline %d (chunk %d), found corrupt leader: sample table and data result in access past end of the file: sample table size %lld + data size %lld larger than file %lld",
                    &[y.into(), cidx.into(), Arg::I64(ddata[0]), Arg::I64(ddata[1]), Arg::I64(fsize)],
                ));
            }
        } else {
            let unpacksize =
                compute_chunk_unpack_size(dw.min.x, miny, cinfo.width, cinfo.height, lpc, part);

            rdcnt += 1;
            if data[rdcnt] < 0 || data[rdcnt] as u64 > part.unpacked_size_per_chunk {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: packed data size says %llu, must be between 0 and %llu",
                    &[
                        y.into(),
                        cidx.into(),
                        Arg::U64(data[rdcnt] as u64),
                        Arg::U64(part.unpacked_size_per_chunk),
                    ],
                ));
            }

            cinfo.data_offset = dataoff;
            cinfo.packed_size = data[rdcnt] as u64;
            cinfo.unpacked_size = unpacksize;
            cinfo.sample_count_data_offset = 0;
            cinfo.sample_count_table_size = 0;

            if fsize > 0 && cinfo.data_offset.wrapping_add(cinfo.packed_size) > fsize as u64 {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: packed size %llu, file offset %llu, size %lld",
                    &[
                        y.into(),
                        cidx.into(),
                        Arg::U64(cinfo.packed_size),
                        Arg::U64(cinfo.data_offset),
                        Arg::I64(fsize),
                    ],
                ));
            }
        }

        self.check_chunk_sizes(part, &cinfo)?;
        Ok(cinfo)
    }

    /// The two checks `exr_read_scanline_chunk_info` and `exr_read_tile_chunk_info` end with
    /// (src/lib/OpenEXRCore/chunk.c:1118-1134 @ v3.5.3).
    fn check_chunk_sizes(&self, part: &Part, cinfo: &ChunkInfo) -> Result<(), Error> {
        if cinfo.packed_size == 0 && cinfo.unpacked_size > 0 {
            return Err(self.report_error(ErrorCode::InvalidArgument, b"Invalid packed size of 0"));
        }
        if part.comp_type == Compression::NONE && cinfo.packed_size != cinfo.unpacked_size {
            return Err(self.print_error(
                ErrorCode::BadChunkLeader,
                "Mismatch between unpacked and packed size with uncompressed data: packed is %llu; unpacked is %llu",
                &[Arg::U64(cinfo.packed_size), Arg::U64(cinfo.unpacked_size)],
            ));
        }
        Ok(())
    }

    /// Port of `exr_read_tile_chunk_info` (src/lib/OpenEXRCore/chunk.c:1139-1514 @ v3.5.3):
    /// the chunk holding tile (`tilex`, `tiley`) of level (`levelx`, `levely`).
    pub fn read_tile_chunk_info(
        &self,
        part_index: i32,
        tilex: i32,
        tiley: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<ChunkInfo, Error> {
        let part = self.part_read_only(part_index)?;
        let mut cinfo = ChunkInfo::default();

        if part.storage_mode != Storage::TILED && part.storage_mode != Storage::DEEP_TILED {
            return Err(self.standard_error(ErrorCode::TileScanMixedapi));
        }

        let cidx = self.validate_and_compute_tile_chunk_off(part, tilex, tiley, levelx, levely)?;

        let Some(AttrValue::TileDesc(tiledesc)) = part.attr(part.tiles).map(|a| a.value()) else {
            unreachable!("checked by validate_and_compute_tile_chunk_off")
        };

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

        cinfo.idx = cidx;
        cinfo.type_ = part.storage_mode.0 as u8;
        cinfo.compression = part.comp_type.0 as u8;
        cinfo.start_x = tilex;
        cinfo.start_y = tiley;
        cinfo.height = tileh;
        cinfo.width = tilew;
        if levelx > 255 || levely > 255 {
            return Err(self.print_error(
                ErrorCode::AttrSizeMismatch,
                "Unable to represent tile level %d, %d in chunk structure",
                &[levelx.into(), levely.into()],
            ));
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

        let (ctable, chunkmin) = self.extract_chunk_table(part_index as usize)?;

        let ntoread: usize = if part.storage_mode == Storage::DEEP_TILED {
            if self.is_multipart { 5 } else { 4 }
        } else if self.is_multipart {
            6
        } else {
            5
        };

        let fsize = self.file_size;
        let mut dataoff = ctable[cidx as usize];

        // known behavior for partial files
        if dataoff == 0 {
            return Err(Error::bare(ErrorCode::IncompleteChunkTable));
        }

        if dataoff < chunkmin || (fsize > 0 && dataoff > fsize as u64) {
            return Err(self.print_error(
                ErrorCode::BadChunkLeader,
                "Corrupt chunk offset table: tile (%d, %d), level (%d, %d), chunk index %d recorded at file offset %llu",
                &[
                    tilex.into(),
                    tiley.into(),
                    levelx.into(),
                    levely.into(),
                    cidx.into(),
                    Arg::U64(dataoff),
                ],
            ));
        }

        let mut raw = [0u8; 24];
        let mut nread: i64 = 0;
        if let Err(e) = dispatch_read(
            &self.core,
            &self.read_fn,
            &mut raw[..ntoread * 4],
            &mut dataoff,
            &mut nread,
            ReadMode::MustReadAll,
        ) {
            return Err(self.print_error(
                e.code,
                "Unable to read information block for tile (%d, %d), level (%d, %d): request %llu bytes from offset %llu, got %llu bytes",
                &[
                    tilex.into(),
                    tiley.into(),
                    levelx.into(),
                    levely.into(),
                    Arg::U64(ntoread as u64 * 4),
                    Arg::U64(ctable[cidx as usize]),
                    Arg::U64(nread as u64),
                ],
            ));
        }
        let mut data = [0i32; 6];
        for (d, b) in data.iter_mut().zip(raw.as_chunks::<4>().0) {
            *d = i32::from_le_bytes(*b);
        }

        let mut tdata: &[i32] = &data;
        if self.is_multipart {
            if part_index != data[0] {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): bad part number (%d, expect %d)",
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        levely.into(),
                        cidx.into(),
                        data[0].into(),
                        part_index.into(),
                    ],
                ));
            }
            tdata = &data[1..];
        }
        let coords = [
            (
                tdata[0],
                tilex,
                "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): bad tile x coordinate (%d, expect %d)",
            ),
            (
                tdata[1],
                tiley,
                "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): bad tile Y coordinate (%d, expect %d)",
            ),
            (
                tdata[2],
                levelx,
                "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): bad tile mip/rip level X (%d, expect %d)",
            ),
            (
                tdata[3],
                levely,
                "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): bad tile mip/rip level Y (%d, expect %d)",
            ),
        ];
        for (found, expect, fmt) in coords {
            if found != expect {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    fmt,
                    &[
                        tilex.into(),
                        tiley.into(),
                        levelx.into(),
                        levely.into(),
                        cidx.into(),
                        found.into(),
                        expect.into(),
                    ],
                ));
            }
        }

        let at = |c: &[Arg<'static>]| {
            let mut v: Vec<Arg<'static>> = vec![
                tilex.into(),
                tiley.into(),
                levelx.into(),
                levely.into(),
                cidx.into(),
            ];
            v.extend_from_slice(c);
            v
        };
        if part.storage_mode == Storage::DEEP_TILED {
            let mut raw = [0u8; 24];
            self.read_all(&mut raw, &mut dataoff)?;
            let mut ddata = [0i64; 3];
            for (d, b) in ddata.iter_mut().zip(raw.as_chunks::<8>().0) {
                *d = i64::from_le_bytes(*b);
            }

            // because of smaller tiles than base tile size, sample table may be smaller than
            // normal tile size, just check that it is at least a multiple of int32_t
            if ddata[0] < 0
                || ddata[0] > i64::from(i32::MAX)
                || (part.comp_type == Compression::NONE && !(ddata[0] as u64).is_multiple_of(4))
                || (ddata[0] == 0 && (ddata[1] != 0 || ddata[2] != 0))
            {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt deep tile (%d, %d), level (%d, %d) (chunk %d): invalid sample table size %lld",
                    &at(&[Arg::I64(ddata[0])]),
                ));
            }

            // not all compressors support 64-bit
            if ddata[1] < 0 || ddata[1] > i64::from(i32::MAX) || (ddata[1] == 0 && ddata[2] != 0) {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt deep tile (%d, %d), level (%d, %d) (chunk %d): invalid packed data size %lld",
                    &at(&[Arg::I64(ddata[1])]),
                ));
            }

            if ddata[2] < 0 || ddata[2] > i64::from(i32::MAX) || (ddata[2] == 0 && ddata[1] != 0) {
                // upstream prints the packed size here
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt deep tile (%d, %d), level (%d, %d) (chunk %d): invalid unpacked size %lld",
                    &at(&[Arg::I64(ddata[1])]),
                ));
            }
            cinfo.sample_count_data_offset = dataoff;
            cinfo.sample_count_table_size = ddata[0] as u64;
            cinfo.packed_size = ddata[1] as u64;
            cinfo.unpacked_size = ddata[2] as u64;
            cinfo.data_offset = dataoff.wrapping_add(ddata[0] as u64);

            if fsize > 0
                && (cinfo
                    .sample_count_data_offset
                    .wrapping_add(cinfo.sample_count_table_size)
                    > fsize as u64
                    || cinfo.data_offset.wrapping_add(cinfo.packed_size) > fsize as u64)
            {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt deep tile (%d, %d), level (%d, %d) (chunk %d): access past end of the file: sample table size %lld + data size %lld larger than file %lld",
                    &at(&[Arg::I64(ddata[0]), Arg::I64(ddata[1]), Arg::I64(fsize)]),
                ));
            }
        } else {
            if tdata[4] < 0 || (tdata[4] as u64) > unpacksize || (tdata[4] == 0 && unpacksize != 0)
            {
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): invalid packed size %d vs unpacked size %llu",
                    &at(&[tdata[4].into(), Arg::U64(unpacksize)]),
                ));
            } else if fsize > 0 {
                let finpos = dataoff.wrapping_add(tdata[4] as u64);
                if finpos > fsize as u64 {
                    return Err(self.print_error(
                        ErrorCode::BadChunkLeader,
                        "Corrupt tile (%d, %d), level (%d, %d) (chunk %d): access past end of file: packed size (%d) at offset %llu vs size of file %lld",
                        &at(&[tdata[4].into(), Arg::U64(dataoff), Arg::I64(fsize)]),
                    ));
                }
            }

            cinfo.packed_size = tdata[4] as u64;
            cinfo.unpacked_size = unpacksize;
            cinfo.data_offset = dataoff;
            cinfo.sample_count_data_offset = 0;
            cinfo.sample_count_table_size = 0;
        }

        self.check_chunk_sizes(part, &cinfo)?;
        Ok(cinfo)
    }
}
