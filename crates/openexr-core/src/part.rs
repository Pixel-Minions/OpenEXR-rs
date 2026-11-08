// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `part.c` (src/lib/OpenEXRCore/part.c:1-715 @ v3.5.3), so far the read side: the
//! part's storage, tile levels and sizes, chunk count and codec levels. `exr_get_count` is in
//! `context.rs`; adding parts, the chunk table and the setters come with Phases 4 and 6.

use crate::attributes::AttrValue;
use crate::cfmt::Arg;
use crate::context::{Context, ContextMode, Error, Part};
use crate::errors::ErrorCode;
use crate::internal_constants::{EXR_REQ_NAME_STR, EXR_REQ_TYPE_STR, EXR_REQ_VERSION_STR};
use crate::internal_ht_quality::is_lossy_htj2k_quality;
use crate::openexr_attr::AttributeType;
use crate::openexr_attr::Storage;
use crate::string::AttrString;

impl Context {
    /// Port of `exr_get_storage` (src/lib/OpenEXRCore/part.c:41-52 @ v3.5.3).
    pub fn get_storage(&self, part_index: i32) -> Result<Storage, Error> {
        Ok(self.part_ref(part_index)?.storage_mode)
    }

    /// The tile checks `exr_get_tile_levels` and its siblings share: the part is tiled, its tile
    /// information computed, and the levels (when given) in range.
    fn tiled_part(&self, part_index: i32, levels: Option<(i32, i32)>) -> Result<&Part, Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode != Storage::TILED && part.storage_mode != Storage::DEEP_TILED {
            return Err(self.standard_error(ErrorCode::TileScanMixedapi));
        }
        if part.tiles.is_none()
            || part.num_tile_levels_x <= 0
            || part.num_tile_levels_y <= 0
            || part.tile_level_tile_count_x.is_empty()
            || part.tile_level_tile_count_y.is_empty()
        {
            return Err(self.print_error(
                ErrorCode::MissingReqAttr,
                "Tile data missing or corrupt",
                &[],
            ));
        }
        if let Some((levelx, levely)) = levels
            && (levelx < 0
                || levely < 0
                || levelx >= part.num_tile_levels_x
                || levely >= part.num_tile_levels_y)
        {
            return Err(self.standard_error(ErrorCode::ArgumentOutOfRange));
        }
        Ok(part)
    }

    /// Port of `exr_get_tile_levels` (src/lib/OpenEXRCore/part.c:221-245 @ v3.5.3).
    pub fn get_tile_levels(&self, part_index: i32) -> Result<(i32, i32), Error> {
        let part = self.tiled_part(part_index, None)?;
        Ok((part.num_tile_levels_x, part.num_tile_levels_y))
    }

    /// Port of `exr_get_tile_counts` (src/lib/OpenEXRCore/part.c:249-284 @ v3.5.3).
    pub fn get_tile_counts(
        &self,
        part_index: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<(i32, i32), Error> {
        let part = self.tiled_part(part_index, Some((levelx, levely)))?;
        Ok((
            part.tile_level_tile_count_x[levelx as usize],
            part.tile_level_tile_count_y[levely as usize],
        ))
    }

    /// Port of `exr_get_tile_sizes` (src/lib/OpenEXRCore/part.c:288-342 @ v3.5.3): the tile size,
    /// or the level's size when the level is smaller than a tile.
    pub fn get_tile_sizes(
        &self,
        part_index: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<(i32, i32), Error> {
        let part = self.tiled_part(part_index, Some((levelx, levely)))?;
        let Some(AttrValue::TileDesc(tiledesc)) = part.attr(part.tiles).map(|a| &a.value) else {
            unreachable!("tiles is a tiledesc")
        };
        let levw = part.tile_level_tile_size_x[levelx as usize];
        let levh = part.tile_level_tile_size_y[levely as usize];
        let tilew = if tiledesc.x_size < levw as u32 {
            tiledesc.x_size as i32
        } else {
            levw
        };
        let tileh = if tiledesc.y_size < levh as u32 {
            tiledesc.y_size as i32
        } else {
            levh
        };
        Ok((tilew, tileh))
    }

    /// Port of `exr_get_level_sizes` (src/lib/OpenEXRCore/part.c:346-384 @ v3.5.3).
    pub fn get_level_sizes(
        &self,
        part_index: i32,
        levelx: i32,
        levely: i32,
    ) -> Result<(i32, i32), Error> {
        let part = self.tiled_part(part_index, Some((levelx, levely)))?;
        Ok((
            part.tile_level_tile_size_x[levelx as usize],
            part.tile_level_tile_size_y[levely as usize],
        ))
    }

    /// Port of `exr_get_chunk_count` (src/lib/OpenEXRCore/part.c:388-438 @ v3.5.3).
    pub fn get_chunk_count(&self, part_index: i32) -> Result<i32, Error> {
        let part = self.part_ref(part_index)?;
        if part.data_window_attr.is_some() {
            match part.storage_mode {
                Storage::TILED | Storage::DEEP_TILED => {
                    if part.tiles.is_some() {
                        return Ok(part.chunk_count);
                    }
                    return Err(self
                        .report_error(ErrorCode::MissingReqAttr, b"Tile data missing or corrupt"));
                }
                Storage::SCANLINE | Storage::DEEP_SCANLINE => {
                    if part.compression.is_some() {
                        return Ok(part.chunk_count);
                    }
                    return Err(self.report_error(
                        ErrorCode::MissingReqAttr,
                        b"Missing scanline chunk compression information",
                    ));
                }
                Storage::UNKNOWN => return Ok(part.chunk_count),
                _ => {}
            }
        }
        Err(self.report_error(
            ErrorCode::MissingReqAttr,
            b"Missing data window for chunk information",
        ))
    }

    /// Port of `exr_get_chunk_table` (src/lib/OpenEXRCore/part.c:447-468 @ v3.5.3): the part's
    /// chunk table, read from the file on first use, and its chunk count. Upstream keeps a
    /// write context's lock on return (U-21); the port has no locks.
    pub fn get_chunk_table(&self, part_index: i32) -> Result<(&[u64], i32), Error> {
        let count = self.get_chunk_count(part_index)?;
        // need to read from the file to get the packed chunk size
        let (table, _chunkmin) = self.extract_chunk_table(part_index as usize)?;
        Ok((table, count))
    }

    /// Port of `exr_validate_chunk_table` (src/lib/OpenEXRCore/part.c:472-502 @ v3.5.3):
    /// `EXR_ERR_INCOMPLETE_CHUNK_TABLE` unless every chunk offset lies after the chunk table
    /// and inside the file. Upstream keeps a write context's lock on return (U-21).
    pub fn validate_chunk_table(&self, part_index: i32) -> Result<(), Error> {
        let part = self.part_ref(part_index)?;
        // need to read from the file to get the packed chunk size
        let (ctable, chunkmin) = self.extract_chunk_table(part_index as usize)?;

        let maxoff = if self.file_size > 0 {
            self.file_size as u64
        } else {
            u64::MAX
        };
        let complete = ctable[..part.chunk_count.max(0) as usize]
            .iter()
            .all(|&cchunk| !(cchunk < chunkmin || cchunk >= maxoff));

        if !complete {
            return Err(Error::bare(ErrorCode::IncompleteChunkTable));
        }
        Ok(())
    }

    /// Port of `exr_get_scanlines_per_chunk` (src/lib/OpenEXRCore/part.c:506-523 @ v3.5.3).
    pub fn get_scanlines_per_chunk(&self, part_index: i32) -> Result<i32, Error> {
        let part = self.part_ref(part_index)?;
        if part.storage_mode == Storage::SCANLINE || part.storage_mode == Storage::DEEP_SCANLINE {
            return Ok(i32::from(part.lines_per_chunk));
        }
        Err(self.standard_error(ErrorCode::ScanTileMixedapi))
    }

    /// Port of `exr_get_chunk_unpacked_size` (src/lib/OpenEXRCore/part.c:527-540 @ v3.5.3).
    pub fn get_chunk_unpacked_size(&self, part_index: i32) -> Result<u64, Error> {
        Ok(self.part_ref(part_index)?.unpacked_size_per_chunk)
    }

    /// Port of `exr_get_zip_compression_level` (src/lib/OpenEXRCore/part.c:544-557 @ v3.5.3).
    pub fn get_zip_compression_level(&self, part_index: i32) -> Result<i32, Error> {
        Ok(self.part_ref(part_index)?.zip_compression_level)
    }

    /// Port of `exr_get_dwa_compression_level` (src/lib/OpenEXRCore/part.c:586-599 @ v3.5.3).
    pub fn get_dwa_compression_level(&self, part_index: i32) -> Result<f32, Error> {
        Ok(self.part_ref(part_index)?.dwa_compression_level)
    }

    /// Port of `exr_get_lossy_htj2k_quality` (src/lib/OpenEXRCore/part.c:635-648 @ v3.5.3).
    pub fn get_lossy_htj2k_quality(&self, part_index: i32) -> Result<f32, Error> {
        Ok(self.part_ref(part_index)?.lossy_htj2k_quality)
    }

    /// Port of `exr_get_zstd_compression_level` (src/lib/OpenEXRCore/part.c:675-688 @ v3.5.3).
    pub fn get_zstd_compression_level(&self, part_index: i32) -> Result<i32, Error> {
        Ok(self.part_ref(part_index)?.zstd_compression_level)
    }

    /// The write-mode check of the setters: `EXR_ERR_NOT_OPEN_WRITE` unless the context is
    /// open for writing or temporary.
    pub(crate) fn require_write(&self) -> Result<(), Error> {
        if self.mode != ContextMode::Write && self.mode != ContextMode::Temporary {
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        Ok(())
    }

    /// `EXR_LOCK_AND_DEFINE_PART` plus the write-mode check, as the setters of `part.c` do.
    fn part_for_write(&mut self, part_index: i32) -> Result<&mut Part, Error> {
        self.part_ref(part_index)?;
        self.require_write()?;
        Ok(&mut self.parts[part_index as usize])
    }

    /// Port of `exr_add_part` (src/lib/OpenEXRCore/part.c:57-219 @ v3.5.3): adds a part of
    /// `type_` named `partname`, with its `type` (and for deep parts `version`) attribute;
    /// returns its index.
    pub fn add_part(&mut self, partname: Option<&str>, type_: Storage) -> Result<i32, Error> {
        let partname = partname.map(crate::string::cstr);
        self.require_write()?;
        let mut partname = partname.map(str::as_bytes);
        let pnamelen = partname.map_or(0, <[u8]>::len);
        if pnamelen >= i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidAttr,
                "Part name '%s': Invalid name length %llu",
                &[
                    Arg::Str(partname.unwrap_or_default()),
                    Arg::U64(pnamelen as u64),
                ],
            ));
        }
        if !self.parts.is_empty() {
            // ensure multi part has at least some name?
            let pn: &[u8] = partname.unwrap_or(b"");
            partname = Some(pn);
            for (pidx, p) in self.parts.iter().enumerate() {
                let Some(AttrValue::String(pname)) = p.attr(p.name).map(|a| &a.value) else {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Part %d missing required attribute 'name' for multi-part file",
                        &[(pidx as i32).into()],
                    ));
                };
                if pn == pname.c_str() {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Each part should have a unique name, part %d and %d attempting to have same name '%s'",
                        &[(pidx as i32).into(), (self.parts.len() as i32).into(), Arg::Str(pn)],
                    ));
                }
            }
        }
        let new_index = self.internal_exr_add_part();
        let (typestr, attrsz): (&[u8], i32) = match type_ {
            Storage::SCANLINE => (b"scanlineimage", 13),
            Storage::TILED => (b"tiledimage", 10),
            Storage::DEEP_SCANLINE => (b"deepscanline", 12),
            Storage::DEEP_TILED => (b"deeptile", 8),
            _ => {
                self.parts.pop(); // internal_exr_revert_add_part
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid storage type %d for new part",
                    &[type_.0.into()],
                ));
            }
        };
        let rv = self.fill_new_part(new_index, partname, pnamelen, type_, typestr, attrsz);
        if rv.is_err() {
            self.parts.pop(); // internal_exr_revert_add_part
            return rv.map(|_| 0);
        }
        if self.parts.len() > 1 {
            self.is_multipart = true;
        }
        self.is_singlepart_tiled =
            !self.has_nonimage_data && self.parts.len() == 1 && type_ == Storage::TILED;
        Ok(new_index as i32)
    }

    /// The attributes `exr_add_part` gives a new part.
    fn fill_new_part(
        &mut self,
        p: usize,
        partname: Option<&[u8]>,
        pnamelen: usize,
        type_: Storage,
        typestr: &[u8],
        attrsz: i32,
    ) -> Result<(), Error> {
        let Context {
            core,
            parts,
            custom_handlers,
            ..
        } = self;
        let part = &mut parts[p];
        part.storage_mode = type_;
        let add = |part: &mut Part, name: &str, t| {
            core.attr_list_add_static_name(
                custom_handlers,
                &mut part.attributes,
                Some(name.as_bytes()),
                t,
                0,
                false,
            )
            .map(|(id, _)| id)
        };
        let id = add(part, EXR_REQ_TYPE_STR, AttributeType::STRING)?;
        part.type_ = Some(id);
        let mut s = AttrString::default();
        core.attr_string_init_static_with_length(&mut s, Some(typestr), attrsz)?;
        part.attributes.get_mut(id).expect("added").value = AttrValue::String(s);
        if let Some(pn) = partname {
            let id = add(part, EXR_REQ_NAME_STR, AttributeType::STRING)?;
            part.name = Some(id);
            let mut s = AttrString::default();
            core.attr_string_create_with_length(&mut s, Some(pn), pnamelen as i32)?;
            part.attributes.get_mut(id).expect("added").value = AttrValue::String(s);
        }
        if type_ == Storage::DEEP_TILED || type_ == Storage::DEEP_SCANLINE {
            let id = add(part, EXR_REQ_VERSION_STR, AttributeType::INT)?;
            part.version = Some(id);
            part.attributes.get_mut(id).expect("added").value = AttrValue::Int(1);
            self.has_nonimage_data = true;
        }
        Ok(())
    }

    /// Port of `exr_set_zip_compression_level` (src/lib/OpenEXRCore/part.c:560-583 @ v3.5.3).
    pub fn set_zip_compression_level(&mut self, part_index: i32, level: i32) -> Result<(), Error> {
        self.part_for_write(part_index)?;
        if level >= -1 && level < 10 {
            self.parts[part_index as usize].zip_compression_level = level;
            return Ok(());
        }
        Err(self.report_error(ErrorCode::InvalidArgument, b"Invalid zip level specified"))
    }

    /// Port of `exr_set_dwa_compression_level` (src/lib/OpenEXRCore/part.c:602-631 @ v3.5.3).
    pub fn set_dwa_compression_level(&mut self, part_index: i32, level: f32) -> Result<(), Error> {
        self.part_for_write(part_index)?;
        // avoid bad math (fp exceptions or whatever) by clamping here
        if level >= 0.0 && level <= 65504.0f32 * 100000.0f32 {
            self.parts[part_index as usize].dwa_compression_level = level;
            return Ok(());
        }
        Err(self.report_error(
            ErrorCode::InvalidArgument,
            b"Invalid dwa quality level specified",
        ))
    }

    /// Port of `exr_set_lossy_htj2k_quality` (src/lib/OpenEXRCore/part.c:651-670 @ v3.5.3).
    pub fn set_lossy_htj2k_quality(&mut self, part_index: i32, level: f32) -> Result<(), Error> {
        self.part_for_write(part_index)?;
        if !is_lossy_htj2k_quality(level) {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid HTJ2K quality level specified, must be between 1 and 150",
            ));
        }
        self.parts[part_index as usize].lossy_htj2k_quality = level;
        Ok(())
    }

    /// Port of `exr_set_zstd_compression_level` (src/lib/OpenEXRCore/part.c:691-715 @ v3.5.3).
    pub fn set_zstd_compression_level(&mut self, part_index: i32, level: i32) -> Result<(), Error> {
        self.part_for_write(part_index)?;
        if level >= 1 && level <= 22 {
            self.parts[part_index as usize].zstd_compression_level = level;
            return Ok(());
        }
        Err(self.report_error(
            ErrorCode::InvalidArgument,
            b"Invalid zstd compression level (valid range is 1-22)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use crate::context::{
        Context, DefaultWriteMode, EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION, Initializer,
    };
    use crate::errors::ErrorCode;
    use crate::openexr_attr::{Compression, PerceptualTreatment, PixelType, Storage};
    use crate::upstream_tests::read::make_temp_dir;

    /// A 1x2 scanline file without compression (two chunks), written by the port; its bytes
    /// and the file offset of its chunk table.
    fn two_chunk_file(path: &str) -> (Vec<u8>, usize) {
        let mut f = Context::start_write(Some(path), DefaultWriteMode::WriteFileDirectly, None)
            .expect("start_write");
        f.add_part(Some("p"), Storage::SCANLINE).expect("add_part");
        f.initialize_required_attr_simple(0, 1, 2, Compression::NONE)
            .expect("required attributes");
        f.add_channel(
            0,
            Some("Y"),
            PixelType::HALF,
            PerceptualTreatment::LOGARITHMIC,
            1,
            1,
        )
        .expect("add_channel");
        f.write_header().expect("write_header");
        let table_off = f.get_chunk_table_offset(0).expect("chunk table offset") as usize;
        for y in 0..2 {
            f.write_scanline_chunk(0, y, Some(&[0, 0x3c]))
                .expect("write_scanline_chunk");
        }
        f.finish().expect("finish");
        (std::fs::read(path).expect("the file"), table_off)
    }

    fn open(path: &str, flags: i32) -> Context {
        Context::start_read(
            Some(path),
            Some(Initializer {
                flags,
                ..Initializer::default()
            }),
        )
        .expect("start_read")
    }

    /// `exr_get_chunk_table` returns the offsets stored in the file; `exr_validate_chunk_table`
    /// accepts them, and refuses an offset past the end of the file when the table is not
    /// rebuilt.
    #[test]
    fn chunk_table_get_and_validate() {
        let tempdir = make_temp_dir();
        let path = format!("{tempdir}chunktable.exr");
        let (bytes, table_off) = two_chunk_file(&path);
        let stored: Vec<u64> = (0..2)
            .map(|i| {
                u64::from_le_bytes(
                    bytes[table_off + 8 * i..table_off + 8 * i + 8]
                        .try_into()
                        .unwrap(),
                )
            })
            .collect();

        let f = open(&path, 0);
        let (table, count) = f.get_chunk_table(0).expect("exr_get_chunk_table");
        assert_eq!((table, count), (&stored[..], 2));
        f.validate_chunk_table(0).expect("exr_validate_chunk_table");
        assert_eq!(
            f.get_chunk_table(1).unwrap_err().code,
            ErrorCode::ArgumentOutOfRange
        );
        assert_eq!(
            f.validate_chunk_table(-1).unwrap_err().code,
            ErrorCode::ArgumentOutOfRange
        );
        drop(f);

        // the second chunk's offset at the end of the file
        let mut bad = bytes.clone();
        bad[table_off + 8..table_off + 16].copy_from_slice(&(bytes.len() as u64).to_le_bytes());
        std::fs::write(&path, &bad).unwrap();

        let f = open(&path, EXR_CONTEXT_FLAG_DISABLE_CHUNK_RECONSTRUCTION);
        assert_eq!(
            f.get_chunk_table(0).unwrap().0,
            &[stored[0], bytes.len() as u64]
        );
        assert_eq!(
            f.validate_chunk_table(0).unwrap_err().code,
            ErrorCode::IncompleteChunkTable
        );
        drop(f);

        // rebuilt from the chunks themselves
        let f = open(&path, 0);
        assert_eq!(f.get_chunk_table(0).unwrap().0, &stored[..]);
        f.validate_chunk_table(0).expect("exr_validate_chunk_table");
        drop(f);
        let _ = std::fs::remove_file(&path);
    }

    /// `exr_start_inplace_header_update` is not implemented upstream: every call is refused.
    #[test]
    fn inplace_header_update_is_refused() {
        let r = Context::start_inplace_header_update(Some("any.exr"), None);
        let Err(e) = r else {
            panic!("exr_start_inplace_header_update is refused")
        };
        assert_eq!(e.code, ErrorCode::InvalidArgument);
    }

    /// `exr_compression_is_lossy`: PXR24, B44, B44A, DWAA, DWAB and LJ2K
    /// (src/lib/OpenEXRCore/compression.c:480-496 @ v3.5.3).
    #[test]
    fn compression_is_lossy() {
        let lossy: Vec<i32> = (-1..16)
            .filter(|&c| crate::compression::compression_is_lossy(Compression(c)))
            .collect();
        assert_eq!(lossy, [5, 6, 7, 8, 9, 12]);
    }
}
