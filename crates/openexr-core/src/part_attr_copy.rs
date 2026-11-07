// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! `exr_copy_unset_attributes` and its `copy_attr` (src/lib/OpenEXRCore/part_attr.c:229-583
//! @ v3.5.3): copy the attributes of one part that another part does not have yet.

use crate::attributes::{AttrId, AttrValue, Attribute, AttributeList};
use crate::cfmt::Arg;
use crate::context::{Context, ContextCore, Error, Part};
use crate::errors::ErrorCode;
use crate::internal_constants::*;
use crate::openexr_attr::{AttributeType, Compression, LineOrder};
use crate::part_attr::ReqSlot;

/// The required attribute names and the part slots they are cached in (the `switch` on the
/// first letter and the `strcmp`s of `copy_attr`).
const REQUIRED: [(&str, ReqSlot); 13] = [
    (EXR_REQ_CHANNELS_STR, |p| &mut p.channels),
    (EXR_REQ_COMP_STR, |p| &mut p.compression),
    (EXR_REQ_CHUNK_COUNT_STR, |p| &mut p.chunk_count_attr),
    (EXR_REQ_DATA_STR, |p| &mut p.data_window_attr),
    (EXR_REQ_DISP_STR, |p| &mut p.display_window_attr),
    (EXR_REQ_LO_STR, |p| &mut p.line_order),
    (EXR_REQ_NAME_STR, |p| &mut p.name),
    (EXR_REQ_PAR_STR, |p| &mut p.pixel_aspect_ratio),
    (EXR_REQ_SCR_WC_STR, |p| &mut p.screen_window_center),
    (EXR_REQ_SCR_WW_STR, |p| &mut p.screen_window_width),
    (EXR_REQ_TILES_STR, |p| &mut p.tiles),
    (EXR_REQ_TYPE_STR, |p| &mut p.type_),
    (EXR_REQ_VERSION_STR, |p| &mut p.version),
];

/// The part slot a required attribute name is cached in, if it is one.
fn required_slot(name: &[u8]) -> Option<ReqSlot> {
    REQUIRED
        .iter()
        .find(|(n, _)| n.as_bytes() == name)
        .map(|&(_, slot)| slot)
}

/// The type a required attribute's value is read as after the copy: by `copy_attr` itself,
/// or (`tiles`) by the tile information it recomputes.
fn read_as(name: &[u8]) -> Option<AttributeType> {
    let is = |c: &str| c.as_bytes() == name;
    if is(EXR_REQ_COMP_STR) {
        Some(AttributeType::COMPRESSION)
    } else if is(EXR_REQ_DATA_STR) || is(EXR_REQ_DISP_STR) {
        Some(AttributeType::BOX2I)
    } else if is(EXR_REQ_LO_STR) {
        Some(AttributeType::LINEORDER)
    } else if is(EXR_REQ_TILES_STR) {
        Some(AttributeType::TILEDESC)
    } else {
        None
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
            if aname == EXR_REQ_DATA_STR.as_bytes() || aname == EXR_REQ_TILES_STR.as_bytes() {
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
                AttrValue::Compression(uc) if aname == EXR_REQ_COMP_STR.as_bytes() => {
                    part.comp_type = Compression(i32::from(*uc));
                }
                AttrValue::Box2i(b) if aname == EXR_REQ_DATA_STR.as_bytes() => {
                    part.data_window = *b
                }
                AttrValue::Box2i(b) if aname == EXR_REQ_DISP_STR.as_bytes() => {
                    part.display_window = *b
                }
                AttrValue::LineOrder(uc) if aname == EXR_REQ_LO_STR.as_bytes() => {
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
/// upstream, which refuses it as an invalid attribute; the port copies it, a deviation
/// (docs/improvements.md, I-9).
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
    /// @ v3.5.3). Upstream counts a lookup failing for another reason than a missing name as
    /// found; the port returns that error, a deviation (docs/improvements.md, I-12).
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
            match found {
                Ok(_) => {}
                Err(e) if e.code == ErrorCode::NoAttrByName => {
                    copy_attr(core, custom_handlers, part, srca, &mut update_tiles)?;
                }
                Err(e) => return Err(e),
            }
        }
        if update_tiles {
            crate::parse_header::internal_exr_compute_tile_information(core, part, true)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::attributes::AttrValue;
    use crate::context::Context;
    use crate::errors::ErrorCode;
    use crate::openexr_attr::{AttributeType, Storage};
    use crate::upstream_tests::general_attr::create_dummy_file;
    use crate::upstream_tests::read::test_image_dir;

    /// An attribute's value without its allocation: a copy owns exactly what it holds, as
    /// upstream's copies do, where a parsed value may hold spare capacity or point into the
    /// header it was read from.
    fn value_of(v: &AttrValue) -> String {
        match v {
            AttrValue::StringVector(sv) => {
                let n = sv.n_strings as usize;
                format!(
                    "{n} {:?}",
                    sv.strings[..n]
                        .iter()
                        .map(|s| s.c_str())
                        .collect::<Vec<_>>()
                )
            }
            AttrValue::Chlist(cl) => {
                let n = cl.num_channels as usize;
                format!("{n} {:?}", &cl.entries[..n])
            }
            AttrValue::String(s) => format!("{} {:?}", s.length, s.c_str()),
            v => format!("{v:?}"),
        }
    }

    /// Every attribute of a file's part arrives in a part that lacks it, with the same type
    /// and value, and the part's cached window and compression follow.
    #[test]
    fn copies_every_unset_attribute() {
        let path = test_image_dir() + "v1.7.test.interleaved.exr";
        let src = Context::start_read(Some(&path), None).expect("exr_start_read");
        let mut dst = create_dummy_file("<copy>");
        dst.copy_unset_attributes(0, &src, 0).unwrap();
        let (sp, dp) = (&src.parts[0], &dst.parts[0]);
        for sa in sp.attributes.entries() {
            let id = dst
                .attr_list_find_by_name(&dp.attributes, Some(sa.name()))
                .expect("copied");
            let da = dp.attributes.get(id).unwrap();
            assert_eq!(da.type_name(), sa.type_name());
            assert_eq!(value_of(da.value()), value_of(sa.value()));
        }
        assert_eq!(dp.data_window, sp.data_window);
        assert_eq!(dp.display_window, sp.display_window);
        assert_eq!(dp.comp_type, sp.comp_type);
        assert_eq!(dp.lineorder, sp.lineorder);
        assert!(dp.channels.is_some() && dp.compression.is_some());
    }

    /// `deepImageState` is copied (docs/improvements.md, I-9); upstream refuses it.
    #[test]
    fn copies_deep_image_state() {
        let mut f = create_dummy_file("<copy deep>");
        f.add_part(Some("other"), Storage::SCANLINE).unwrap();
        let id = f
            .attr_declare(0, Some("deepImageState"), AttributeType::DEEP_IMAGE_STATE)
            .unwrap();
        *f.attr_value_mut(0, id) = AttrValue::DeepImageState(3);
        f.copy_unset_attributes_within(1, 0).unwrap();
        let p = &f.parts[1];
        let id = f
            .attr_list_find_by_name(&p.attributes, Some(b"deepImageState"))
            .expect("copied");
        assert!(matches!(
            p.attributes.get(id).unwrap().value(),
            AttrValue::DeepImageState(3)
        ));
    }

    /// A required name holding another type is refused before anything is added
    /// (docs/improvements.md, U-6).
    #[test]
    fn refuses_a_required_name_of_another_type() {
        let mut src = create_dummy_file("<copy src>");
        src.attr_set_int(0, Some("compression"), 1).unwrap();
        let mut dst = create_dummy_file("<copy dst>");
        assert_eq!(
            dst.copy_unset_attributes(0, &src, 0).unwrap_err().code,
            ErrorCode::AttrTypeMismatch
        );
        assert!(dst.parts[0].compression.is_none());
    }
}
