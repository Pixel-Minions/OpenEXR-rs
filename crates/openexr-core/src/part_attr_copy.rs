// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! `exr_copy_unset_attributes` and its `copy_attr` (src/lib/OpenEXRCore/part_attr.c:229-583
//! @ v3.5.3): copy the attributes of one part that another part does not have yet.

use crate::attributes::{AttrId, AttrValue, Attribute, AttributeList};
use crate::cfmt::Arg;
use crate::context::{Context, ContextCore, Error, Part};
use crate::errors::ErrorCode;
use crate::openexr_attr::{AttributeType, Compression, LineOrder};
use crate::part_attr::ReqSlot;

/// The part slot a required attribute name is cached in, if it is one (the `switch` on the
/// first letter and the `strcmp`s of `copy_attr`).
fn required_slot(name: &[u8]) -> Option<ReqSlot> {
    Some(match name {
        b"channels" => |p| &mut p.channels,
        b"compression" => |p| &mut p.compression,
        b"chunkCount" => |p| &mut p.chunk_count_attr,
        b"dataWindow" => |p| &mut p.data_window_attr,
        b"displayWindow" => |p| &mut p.display_window_attr,
        b"lineOrder" => |p| &mut p.line_order,
        b"name" => |p| &mut p.name,
        b"pixelAspectRatio" => |p| &mut p.pixel_aspect_ratio,
        b"screenWindowCenter" => |p| &mut p.screen_window_center,
        b"screenWindowWidth" => |p| &mut p.screen_window_width,
        b"tiles" => |p| &mut p.tiles,
        b"type" => |p| &mut p.type_,
        b"version" => |p| &mut p.version,
        _ => return None,
    })
}

/// The type `copy_attr` reads a required attribute's value as, where it reads one.
fn read_as(name: &[u8]) -> Option<AttributeType> {
    match name {
        b"compression" => Some(AttributeType::COMPRESSION),
        b"dataWindow" | b"displayWindow" => Some(AttributeType::BOX2I),
        b"lineOrder" => Some(AttributeType::LINEORDER),
        _ => None,
    }
}

/// Port of `copy_attr` (src/lib/OpenEXRCore/part_attr.c:229-524 @ v3.5.3): adds a copy of
/// `srca` to `part`, caching a required attribute in its slot. A copy that fails is removed
/// again. A required attribute whose value upstream reads as its required type, but which
/// has another type, is refused (docs/improvements.md, U-6).
fn copy_attr(
    core: &ContextCore,
    handlers: &AttributeList,
    part: &mut Part,
    srca: &Attribute,
    update_tiles: &mut bool,
) -> Result<(), Error> {
    let aname = srca.name();
    let t = srca.attr_type();
    if read_as(aname).is_some_and(|req| req != t) {
        return Err(core.standard_error(ErrorCode::AttrTypeMismatch));
    }
    let id = match required_slot(aname) {
        Some(slot) => {
            if aname == b"dataWindow" || aname == b"tiles" {
                *update_tiles = true;
            }
            let (id, _) = core.attr_list_add_static_name(
                handlers,
                &mut part.attributes,
                Some(aname),
                t,
                0,
                false,
            )?;
            *slot(part) = Some(id);
            match srca.value() {
                AttrValue::Compression(uc) if aname == b"compression" => {
                    part.comp_type = Compression(i32::from(*uc));
                }
                AttrValue::Box2i(b) if aname == b"dataWindow" => part.data_window = *b,
                AttrValue::Box2i(b) if aname == b"displayWindow" => part.display_window = *b,
                AttrValue::LineOrder(uc) if aname == b"lineOrder" => {
                    part.lineorder = LineOrder(i32::from(*uc));
                }
                _ => {}
            }
            id
        }
        None => {
            core.attr_list_add(handlers, &mut part.attributes, Some(aname), t, 0, false)?
                .0
        }
    };

    let rv = copy_value(
        core,
        &mut part.attributes.get_mut(id).expect("added").value,
        srca,
    );
    if rv.is_err() {
        // the required attributes are cached in the part; don't leave any of them pointing at
        // the attribute that is about to be freed
        let cached: [ReqSlot; 13] = [
            |p| &mut p.channels,
            |p| &mut p.compression,
            |p| &mut p.data_window_attr,
            |p| &mut p.display_window_attr,
            |p| &mut p.line_order,
            |p| &mut p.pixel_aspect_ratio,
            |p| &mut p.screen_window_center,
            |p| &mut p.screen_window_width,
            |p| &mut p.tiles,
            |p| &mut p.name,
            |p| &mut p.type_,
            |p| &mut p.version,
            |p| &mut p.chunk_count_attr,
        ];
        for slot in cached {
            if *slot(part) == Some(id) {
                *slot(part) = None;
            }
        }
        // the attribute was just added, so it is in the list
        let _ = core.attr_list_remove(&mut part.attributes, Some(id));
    }
    rv
}

/// The value copy of `copy_attr` (src/lib/OpenEXRCore/part_attr.c:436-496 @ v3.5.3), into the
/// zeroed value of the attribute just added with `srca`'s type. `deepImageState` has no case
/// upstream and is refused as an invalid attribute.
fn copy_value(core: &ContextCore, dst: &mut AttrValue, srca: &Attribute) -> Result<(), Error> {
    match (dst, srca.value()) {
        (AttrValue::Bytes(d), AttrValue::Bytes(s)) => core.attr_bytes_copy(d, s),
        (AttrValue::Chlist(d), AttrValue::Chlist(s)) => core.attr_chlist_duplicate(d, s),
        (AttrValue::FloatVector(d), AttrValue::FloatVector(s)) => {
            core.attr_float_vector_create(d, s.arr.as_deref(), s.length)
        }
        (AttrValue::Preview(d), AttrValue::Preview(s)) => {
            core.attr_preview_create(d, s.width, s.height, s.rgba.as_deref())
        }
        (AttrValue::String(d), AttrValue::String(s)) => {
            core.attr_string_create_with_length(d, s.str.as_deref(), s.length)
        }
        (AttrValue::StringVector(d), AttrValue::StringVector(s)) => {
            core.attr_string_vector_copy(d, s)
        }
        (AttrValue::Opaque(d), AttrValue::Opaque(s)) => core.attr_opaquedata_copy(d, s),
        (_, AttrValue::DeepImageState(_)) => Err(core.standard_error(ErrorCode::InvalidAttr)),
        (d, s) => {
            *d = s.clone();
            Ok(())
        }
    }
}

impl Context {
    /// Port of `exr_copy_unset_attributes` (src/lib/OpenEXRCore/part_attr.c:528-583 @ v3.5.3):
    /// copies each attribute of part `src_part_index` of `source` that part `part_index` does
    /// not have. (A NULL `source` is n/a, PLAN.md O8; to copy within this context, see
    /// [`Context::copy_unset_attributes_within`].)
    pub fn copy_unset_attributes(
        &mut self,
        part_index: i32,
        source: &Context,
        src_part_index: i32,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        if src_part_index < 0 || src_part_index as usize >= source.parts.len() {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "Source part index (%d) out of range",
                &[Arg::I32(src_part_index)],
            ));
        }
        let src = &source.parts[src_part_index as usize].attributes;
        self.copy_unset_from(part_index, src)
    }

    /// `exr_copy_unset_attributes` with `source == ctxt`: from another part of this context.
    /// A part copied onto itself already has every attribute, so nothing changes.
    pub fn copy_unset_attributes_within(
        &mut self,
        part_index: i32,
        src_part_index: i32,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        if src_part_index < 0 || src_part_index as usize >= self.parts.len() {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "Source part index (%d) out of range",
                &[Arg::I32(src_part_index)],
            ));
        }
        if src_part_index == part_index {
            return Ok(());
        }
        let src = std::mem::take(&mut self.parts[src_part_index as usize].attributes);
        let rv = self.copy_unset_from(part_index, &src);
        self.parts[src_part_index as usize].attributes = src;
        rv
    }

    /// The loop of `exr_copy_unset_attributes` (src/lib/OpenEXRCore/part_attr.c:553-578
    /// @ v3.5.3). A lookup failing for another reason than a missing name counts as found,
    /// as upstream counts it.
    fn copy_unset_from(&mut self, part_index: i32, src: &AttributeList) -> Result<(), Error> {
        let Context {
            core,
            parts,
            custom_handlers,
            ..
        } = self;
        let part = &mut parts[part_index as usize];
        let mut update_tiles = false;
        for srca in src.entries() {
            let found: Result<AttrId, Error> =
                core.attr_list_find_by_name(&part.attributes, Some(srca.name()));
            if let Err(e) = found
                && e.code == ErrorCode::NoAttrByName
            {
                copy_attr(core, custom_handlers, part, srca, &mut update_tiles)?;
            }
        }
        if update_tiles {
            crate::parse_header::internal_exr_compute_tile_information(core, part, true)?;
        }
        Ok(())
    }
}
