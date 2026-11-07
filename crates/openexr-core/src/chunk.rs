// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `chunk.c` (src/lib/OpenEXRCore/chunk.c:1-2428 @ v3.5.3), so far its read side:
//! the chunk table (reading it, validating it, and rebuilding it from the chunk leaders when a
//! file's table is incomplete) and the raw chunk reads. The chunk info reads are in
//! `chunk_info.rs`; the chunk writes come with Phase 6.

use crate::attributes::AttrValue;
use crate::cfmt::Arg;
use crate::chunk_info::ChunkInfo;
use crate::context::{ChunkTable, Context, Error, Part, ReadMode, dispatch_read};
use crate::errors::ErrorCode;
use crate::internal_xdr::one_to_native64;
use crate::openexr_attr::{Compression, LineOrder, Storage, TileLevelMode};

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
        let corrupt = || {
            self.print_error(
                ErrorCode::MissingReqAttr,
                "Tile descriptor data missing or corrupt",
                &[],
            )
        };
        // upstream checks the level arrays for NULL; the port also checks they hold the level
        // counts, which the tile information always makes them (docs/improvements.md, U-8)
        let (nlx, nly) = (part.num_tile_levels_x, part.num_tile_levels_y);
        let tiledesc = match part.attr(part.tiles).map(|a| a.value()) {
            Some(AttrValue::TileDesc(t))
                if nlx > 0
                    && nly > 0
                    && part.tile_level_tile_count_x.len() >= nlx as usize
                    && part.tile_level_tile_size_x.len() >= nlx as usize
                    && part.tile_level_tile_count_y.len() >= nly as usize
                    && part.tile_level_tile_size_y.len() >= nly as usize =>
            {
                *t
            }
            _ => return Err(corrupt()),
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
            // single level and mipmap tiles have as many y levels as x levels
            let Some(&numy) = cy.get(levelx as usize) else {
                return Err(corrupt());
            };
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
            // a part has 1 to 256 lines per chunk (or -1 for an unknown compression, which
            // the header refuses); 0 would trap upstream (docs/improvements.md, U-8)
            if part.lines_per_chunk == 0 {
                return Err(self.standard_error(ErrorCode::BadChunkLeader));
            }
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

impl Context {
    /// The checks `exr_read_chunk` and `exr_read_deep_chunk` share
    /// (src/lib/OpenEXRCore/chunk.c:1533-1549 @ v3.5.3).
    fn check_chunk_block_info(&self, part: &Part, cinfo: &ChunkInfo) -> Result<(), Error> {
        if cinfo.idx < 0 || cinfo.idx >= part.chunk_count {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "invalid chunk index (%d) vs part chunk count %d",
                &[cinfo.idx.into(), part.chunk_count.into()],
            ));
        }
        if cinfo.type_ != part.storage_mode.0 as u8 {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"mismatched storage type for chunk block info",
            ));
        }
        if cinfo.compression != part.comp_type.0 as u8 {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"mismatched compression type for chunk block info",
            ));
        }
        Ok(())
    }

    /// Port of `exr_read_chunk` (src/lib/OpenEXRCore/chunk.c:1516-1585 @ v3.5.3): reads the
    /// chunk's packed bytes into the start of `packed_data`. Uncompressed chunks may be short
    /// in the file; the rest is zeroed. A buffer shorter than `packed_size` is refused
    /// (docs/improvements.md, U-2).
    pub fn read_chunk(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
        packed_data: Option<&mut [u8]>,
    ) -> Result<(), Error> {
        let part = self.part_read_only(part_index)?;
        let toread = cinfo.packed_size;
        let packed_data = match packed_data {
            None if toread > 0 => return Err(self.standard_error(ErrorCode::InvalidArgument)),
            Some(b) if (b.len() as u64) < toread => {
                return Err(self.standard_error(ErrorCode::InvalidArgument));
            }
            p => p,
        };
        self.check_chunk_block_info(part, cinfo)?;

        let mut dataoffset = cinfo.data_offset;
        if self.file_size > 0 && dataoffset > self.file_size as u64 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "chunk block info data offset (%llu) past end of file (%lld)",
                &[Arg::U64(dataoffset), Arg::I64(self.file_size)],
            ));
        }

        // allow a short read if uncompressed
        let rmode = if part.comp_type == Compression::NONE {
            ReadMode::AllowShortRead
        } else {
            ReadMode::MustReadAll
        };

        if toread > 0 {
            let buf = &mut packed_data.expect("checked")[..toread as usize];
            let mut nread: i64 = 0;
            let rv = dispatch_read(
                &self.core,
                &self.read_fn,
                buf,
                &mut dataoffset,
                &mut nread,
                rmode,
            );
            if rmode == ReadMode::AllowShortRead && nread >= 0 && nread < toread as i64 {
                buf[nread as usize..].fill(0);
            }
            rv
        } else {
            Ok(())
        }
    }

    /// Port of `exr_read_deep_chunk` (src/lib/OpenEXRCore/chunk.c:1589-1672 @ v3.5.3): reads
    /// the sample count table into `sample_data` and the packed data into `packed_data`, each
    /// when given, both exactly as stored in the file. A buffer shorter than what it is to
    /// receive is refused (docs/improvements.md, U-2).
    pub fn read_deep_chunk(
        &self,
        part_index: i32,
        cinfo: &ChunkInfo,
        packed_data: Option<&mut [u8]>,
        sample_data: Option<&mut [u8]>,
    ) -> Result<(), Error> {
        let part = self.part_read_only(part_index)?;
        self.check_chunk_block_info(part, cinfo)?;

        if self.file_size > 0 && cinfo.sample_count_data_offset > self.file_size as u64 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "chunk block info sample count offset (%llu) past end of file (%lld)",
                &[
                    Arg::U64(cinfo.sample_count_data_offset),
                    Arg::I64(self.file_size),
                ],
            ));
        }
        if self.file_size > 0 && cinfo.data_offset > self.file_size as u64 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "chunk block info data offset (%llu) past end of file (%lld)",
                &[Arg::U64(cinfo.data_offset), Arg::I64(self.file_size)],
            ));
        }

        let short = |b: &Option<&mut [u8]>, n: u64| {
            b.as_ref().is_some_and(|b| n > 0 && (b.len() as u64) < n)
        };
        if short(&sample_data, cinfo.sample_count_table_size)
            || short(&packed_data, cinfo.packed_size)
        {
            return Err(self.standard_error(ErrorCode::InvalidArgument));
        }

        let rmode = ReadMode::MustReadAll;
        if let Some(sample_data) = sample_data
            && cinfo.sample_count_table_size > 0
        {
            let mut dataoffset = cinfo.sample_count_data_offset;
            let mut nread: i64 = 0;
            let buf = &mut sample_data[..cinfo.sample_count_table_size as usize];
            dispatch_read(
                &self.core,
                &self.read_fn,
                buf,
                &mut dataoffset,
                &mut nread,
                rmode,
            )?;
        }

        // Both buffers are returned exactly as stored in the file, like exr_read_chunk: the
        // sample count table may be compressed, and when it isn't, its counts are
        // little-endian. The decode pipeline (unpack_sample_table) does the conversion to
        // native order, and the raw chunk readers in the C++ API copy these bytes verbatim.

        if let Some(packed_data) = packed_data
            && cinfo.packed_size > 0
        {
            let mut dataoffset = cinfo.data_offset;
            let mut nread: i64 = 0;
            let buf = &mut packed_data[..cinfo.packed_size as usize];
            dispatch_read(
                &self.core,
                &self.read_fn,
                buf,
                &mut dataoffset,
                &mut nread,
                rmode,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::context::Context;
    use crate::upstream_tests::read::test_image_dir;

    /// `exr_read_chunk` hands back the file's bytes at the chunk's data offset, as read here
    /// with `std::fs`.
    #[test]
    fn read_chunk_returns_the_file_bytes() {
        let path = test_image_dir() + "v1.7.test.interleaved.exr";
        let file = std::fs::read(&path).expect("test image");
        let f = Context::start_read(Some(&path), None).expect("exr_start_read");
        let dw = f.get_data_window(0).unwrap();
        for y in [dw.min.y, dw.max.y] {
            let cinfo = f.read_scanline_chunk_info(0, y).unwrap();
            let mut buf = vec![0xAAu8; cinfo.packed_size as usize];
            f.read_chunk(0, &cinfo, Some(&mut buf)).unwrap();
            let at = cinfo.data_offset as usize;
            assert_eq!(buf, file[at..at + cinfo.packed_size as usize]);
        }
    }
}
