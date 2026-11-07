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
