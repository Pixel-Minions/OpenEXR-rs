// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The chunk info half of `chunk.c` (src/lib/OpenEXRCore/chunk.c:676-1514 @ v3.5.3):
//! `exr_chunk_info_t`, `exr_read_scanline_chunk_info` and `exr_read_tile_chunk_info`.
//!
//! C's `exr_chunk_info_t*` out-argument becomes the return value. Upstream fills it as it
//! goes, so after a failure the caller's struct holds the fields set before the failing check;
//! the port returns only the error (PLAN.md O8).

use crate::attributes::AttrValue;
use crate::cfmt::Arg;
use crate::channel_list::AttrChlistEntry;
use crate::context::{Context, ContextMode, Error, Part, ReadMode, dispatch_read};
use crate::errors::ErrorCode;
use crate::internal_util::{compute_sampled_height, compute_sampled_width};
use crate::openexr_attr::{Compression, PixelType, Storage};

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
pub(crate) fn part_channels(part: &Part) -> &[AttrChlistEntry] {
    match part.attr(part.channels).map(|a| a.value()) {
        Some(AttrValue::Chlist(cl)) => &cl.entries[..cl.num_channels.max(0) as usize],
        _ => &[],
    }
}

/// Port of `compute_chunk_unpack_size` (src/lib/OpenEXRCore/chunk.c:676-700 @ v3.5.3).
pub(crate) fn compute_chunk_unpack_size(
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
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: part says %d, expected %d",
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
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: unsupported unpacked data size %lld",
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
                    "Preparing to read scanline %d (chunk %d), found corrupt leader: sample table and data result in access past end of the file: sample table size %lld + data size %lld larger than file %lld",
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
                // upstream prints the packed size, ddata[1], here; the port prints the
                // unpacked size it refuses, a deviation (docs/improvements.md, I-7)
                return Err(self.print_error(
                    ErrorCode::BadChunkLeader,
                    "Corrupt deep tile (%d, %d), level (%d, %d) (chunk %d): invalid unpacked size %lld",
                    &at(&[Arg::I64(ddata[2])]),
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

#[cfg(test)]
mod tests {
    use crate::context::{Context, DefaultWriteMode};
    use crate::errors::ErrorCode;
    use crate::openexr_attr::{Compression, PerceptualTreatment, PixelType, Storage};
    use crate::upstream_tests::read::make_temp_dir;

    /// The message of a refused call.
    fn message<T: std::fmt::Debug>(
        r: Result<T, crate::context::Error>,
    ) -> (crate::errors::ExrResult, String) {
        let e = r.expect_err("the call is refused");
        (
            e.code,
            String::from_utf8_lossy(e.message.as_deref().unwrap_or(&[])).into_owned(),
        )
    }

    /// A 2x2 deep scanline file without compression, one half channel, one sample per pixel,
    /// written by the port; the file's bytes and the offset of chunk 0.
    fn deep_scanline_file(path: &str) -> (Vec<u8>, usize) {
        let mut f = Context::start_write(Some(path), DefaultWriteMode::WriteFileDirectly, None)
            .expect("start_write");
        f.add_part(Some("deep"), Storage::DEEP_SCANLINE)
            .expect("add_part");
        f.initialize_required_attr_simple(0, 2, 2, Compression::NONE)
            .expect("required attributes");
        f.add_channel(
            0,
            Some("A"),
            PixelType::HALF,
            PerceptualTreatment::LOGARITHMIC,
            1,
            1,
        )
        .expect("add_channel");
        f.write_header().expect("write_header");
        let table: Vec<u8> = [1u32, 2].iter().flat_map(|v| v.to_le_bytes()).collect();
        for y in 0..2 {
            f.write_deep_scanline_chunk(0, y, Some(&[0, 0x3c, 0, 0x3c]), 4, Some(&table))
                .expect("write_deep_scanline_chunk");
        }
        f.finish().expect("finish");
        let bytes = std::fs::read(path).expect("the file");
        let r = Context::start_read(Some(path), None).expect("start_read");
        let table_off = r.get_chunk_table_offset(0).expect("chunk table") as usize;
        let chunk0 =
            u64::from_le_bytes(bytes[table_off..table_off + 8].try_into().unwrap()) as usize;
        (bytes, chunk0)
    }

    /// D-6: a deep scanline chunk's corrupt unpacked size and its sizes past the end of the
    /// file are reported as "Preparing to read scanline" (upstream: "Preparing to scanline").
    #[test]
    fn deep_scanline_leader_messages() {
        let tempdir = make_temp_dir();
        let path = format!("{tempdir}deepscan.exr");
        let (bytes, chunk0) = deep_scanline_file(&path);
        // the leader: y, then the table, packed and unpacked sizes
        let at = |i: usize| chunk0 + 4 + 8 * i;

        let mut bad = bytes.clone();
        bad[at(2)..at(2) + 8].copy_from_slice(&(-1i64).to_le_bytes());
        std::fs::write(&path, &bad).unwrap();
        let f = Context::start_read(Some(&path), None).expect("start_read");
        assert_eq!(
            message(f.read_scanline_chunk_info(0, 0)),
            (ErrorCode::BadChunkLeader.into(), "Preparing to read scanline 0 (chunk 0), found corrupt leader: unsupported unpacked data size -1".to_string())
        );

        let mut bad = bytes.clone();
        bad[at(1)..at(1) + 8].copy_from_slice(&1_000_000i64.to_le_bytes());
        std::fs::write(&path, &bad).unwrap();
        let f = Context::start_read(Some(&path), None).expect("start_read");
        assert_eq!(
            message(f.read_scanline_chunk_info(0, 0)),
            (
                ErrorCode::BadChunkLeader.into(),
                format!(
                    "Preparing to read scanline 0 (chunk 0), found corrupt leader: sample table and data result in access past end of the file: sample table size 8 + data size 1000000 larger than file {}",
                    bad.len()
                )
            )
        );
        let _ = std::fs::remove_file(&path);
    }

    /// D-3: a deep tile's corrupt unpacked size is printed (upstream prints the packed size).
    #[test]
    fn deep_tile_unpacked_size_message() {
        use crate::upstream_tests::deep::DEEP_ZSTD_TILE_DATA;

        let tempdir = make_temp_dir();
        let path = format!("{tempdir}deeptile.exr");
        let mut bad = DEEP_ZSTD_TILE_DATA.to_vec();
        // the tile's leader (at 0x18e): tile x, y and levels, then the table, packed and
        // unpacked sizes
        let unpacked = 0x18e + 16 + 16;
        assert_eq!(
            i64::from_le_bytes(bad[unpacked..unpacked + 8].try_into().unwrap()),
            600
        );
        bad[unpacked..unpacked + 8].copy_from_slice(&(-1i64).to_le_bytes());
        std::fs::write(&path, &bad).unwrap();
        let f = Context::start_read(Some(&path), None).expect("start_read");
        assert_eq!(
            message(f.read_tile_chunk_info(0, 0, 0, 0, 0)),
            (
                ErrorCode::BadChunkLeader.into(),
                "Corrupt deep tile (0, 0), level (0, 0) (chunk 0): invalid unpacked size -1"
                    .to_string()
            )
        );
        let _ = std::fs::remove_file(&path);
    }
}
