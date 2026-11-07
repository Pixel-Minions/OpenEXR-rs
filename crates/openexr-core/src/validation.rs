// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `validation.c` (src/lib/OpenEXRCore/validation.c:1-886 @ v3.5.3): the checks of a
//! part's header after it is read (and, from Phase 6, before it is written).

use crate::attributes::{AttrId, AttrValue, AttributeList};
use crate::cfmt::Arg;
use crate::compression::compression_is_valid_for_deep;
use crate::context::{Context, ContextCore, ContextMode, Error, Part};
use crate::errors::ErrorCode;
use crate::internal_constants::EXR_REQ_VERSION_STR;
use crate::openexr_attr::*;
use crate::parse_header::internal_exr_compute_tile_information;

/// The value of a required-attribute slot.
fn value(part: &Part, id: Option<AttrId>) -> Option<&AttrValue> {
    part.attr(id).map(|a| &a.value)
}

/// Adds a built-in attribute with a static name for a default, as `validate_req_attr` does.
fn add_default(
    core: &ContextCore,
    handlers: &AttributeList,
    part: &mut Part,
    name: &str,
    t: AttributeType,
    v: AttrValue,
) -> Result<AttrId, Error> {
    let (id, _) = core.attr_list_add_static_name(
        handlers,
        &mut part.attributes,
        Some(name.as_bytes()),
        t,
        0,
        false,
    )?;
    part.attributes.get_mut(id).expect("added").value = v;
    Ok(id)
}

/// Port of `validate_req_attr` (src/lib/OpenEXRCore/validation.c:17-277 @ v3.5.3): every
/// required attribute is present with its type, or, when `adddefault`, given its default.
fn validate_req_attr(ctxt: &mut Context, p: usize, adddefault: bool) -> Result<(), Error> {
    let is_multipart = ctxt.is_multipart;
    let has_nonimage_data = ctxt.has_nonimage_data;
    let strict_header = ctxt.strict_header;
    let Context {
        core,
        parts,
        custom_handlers,
        ..
    } = ctxt;
    let core: &ContextCore = core;
    let curpart = &mut parts[p];
    let missing = |what: &str| -> Error { core.print_error(ErrorCode::MissingReqAttr, what, &[]) };
    let mismatch =
        |what: &str| -> Error { core.print_error(ErrorCode::AttrTypeMismatch, what, &[]) };

    match value(curpart, curpart.compression) {
        None => {
            if !adddefault {
                return Err(missing("'compression' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "compression",
                AttributeType::COMPRESSION,
                AttrValue::Compression(Compression::ZIP.0 as u8),
            )?;
            curpart.compression = Some(id);
            curpart.comp_type = Compression::ZIP;
        }
        Some(AttrValue::Compression(_)) => {}
        Some(_) => return Err(mismatch("'compression' attribute has wrong data type")),
    }

    let defdw = Box2i {
        min: V2i { x: 0, y: 0 },
        max: V2i { x: 63, y: 63 },
    };
    match value(curpart, curpart.data_window_attr) {
        None => {
            if !adddefault {
                return Err(missing("'dataWindow' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "dataWindow",
                AttributeType::BOX2I,
                AttrValue::Box2i(defdw),
            )?;
            curpart.data_window_attr = Some(id);
            curpart.data_window = defdw;
            internal_exr_compute_tile_information(core, curpart, true)?;
        }
        Some(AttrValue::Box2i(_)) => {}
        Some(_) => return Err(mismatch("'dataWindow' attribute has wrong data type")),
    }

    match value(curpart, curpart.display_window_attr) {
        None => {
            if !adddefault {
                return Err(missing("'displayWindow' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "displayWindow",
                AttributeType::BOX2I,
                AttrValue::Box2i(defdw),
            )?;
            curpart.display_window_attr = Some(id);
            curpart.display_window = defdw;
        }
        Some(AttrValue::Box2i(_)) => {}
        Some(_) => return Err(mismatch("'displayWindow' attribute has wrong data type")),
    }

    match value(curpart, curpart.line_order) {
        None => {
            if !adddefault {
                return Err(missing("'lineOrder' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "lineOrder",
                AttributeType::LINEORDER,
                AttrValue::LineOrder(LineOrder::INCREASING_Y.0 as u8),
            )?;
            curpart.line_order = Some(id);
            curpart.lineorder = LineOrder::INCREASING_Y;
        }
        Some(AttrValue::LineOrder(_)) => {}
        Some(_) => return Err(mismatch("'lineOrder' attribute has wrong data type")),
    }

    match value(curpart, curpart.pixel_aspect_ratio) {
        None => {
            if !adddefault {
                return Err(missing("'pixelAspectRatio' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "pixelAspectRatio",
                AttributeType::FLOAT,
                AttrValue::Float(1.0),
            )?;
            curpart.pixel_aspect_ratio = Some(id);
        }
        Some(AttrValue::Float(_)) => {}
        Some(_) => return Err(mismatch("'pixelAspectRatio' attribute has wrong data type")),
    }

    match value(curpart, curpart.screen_window_center) {
        None => {
            if !adddefault {
                return Err(missing("'screenWindowCenter' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "screenWindowCenter",
                AttributeType::V2F,
                AttrValue::V2f(V2f { x: 0.0, y: 0.0 }),
            )?;
            curpart.screen_window_center = Some(id);
        }
        Some(AttrValue::V2f(_)) => {}
        Some(_) => {
            return Err(mismatch(
                "'screenWindowCenter' attribute has wrong data type",
            ));
        }
    }

    match value(curpart, curpart.screen_window_width) {
        None => {
            if !adddefault {
                return Err(missing("'screenWindowWidth' attribute not found"));
            }
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                "screenWindowWidth",
                AttributeType::FLOAT,
                AttrValue::Float(1.0),
            )?;
            curpart.screen_window_width = Some(id);
        }
        Some(AttrValue::Float(_)) => {}
        Some(_) => {
            return Err(mismatch(
                "'screenWindowWidth' attribute has wrong data type, expect float",
            ));
        }
    }

    if is_multipart || has_nonimage_data {
        if is_multipart {
            match value(curpart, curpart.name) {
                None => return Err(missing("'name' attribute for multipart file not found")),
                Some(AttrValue::String(_)) => {}
                Some(_) => {
                    return Err(mismatch(
                        "'name' attribute has wrong data type, expect string",
                    ));
                }
            }
        }
        match value(curpart, curpart.type_) {
            None => return Err(missing("'type' attribute for v2+ file not found")),
            Some(AttrValue::String(_)) => {}
            Some(_) => {
                return Err(mismatch(
                    "'type' attribute has wrong data type, expect string",
                ));
            }
        }
        if has_nonimage_data && curpart.version.is_none() {
            if !adddefault {
                return Err(missing("'version' attribute for deep file not found"));
            }
            // upstream does not check this add's result
            let id = add_default(
                core,
                custom_handlers,
                curpart,
                EXR_REQ_VERSION_STR,
                AttributeType::INT,
                AttrValue::Int(1),
            )?;
            curpart.version = Some(id);
        }
        if strict_header && curpart.chunk_count_attr.is_none() {
            return Err(missing(
                "'chunkCount' attribute for multipart / deep file not found",
            ));
        }
    }
    Ok(())
}

/// Port of `validate_image_dimensions` (src/lib/OpenEXRCore/validation.c:281-382 @ v3.5.3).
fn validate_image_dimensions(ctxt: &Context, curpart: &Part) -> Result<(), Error> {
    // sanity check the various parts...
    let k_large_val: i64 = i64::from(i32::MAX / 2);
    let dw = curpart.data_window;
    let dspw = curpart.display_window;
    let maxw = ctxt.max_image_w;
    let maxh = ctxt.max_image_h;
    let float_of = |id| match value(curpart, id) {
        Some(AttrValue::Float(f)) => *f,
        _ => 0.0,
    };
    let par = float_of(curpart.pixel_aspect_ratio);
    let sww = float_of(curpart.screen_window_width);
    let w = i64::from(dw.max.x) - i64::from(dw.min.x) + 1;
    let h = i64::from(dw.max.y) - i64::from(dw.min.y) + 1;
    let bad = |b: &Box2i| {
        b.min.x > b.max.x
            || b.min.y > b.max.y
            || i64::from(b.min.x) <= -k_large_val
            || i64::from(b.min.y) <= -k_large_val
            || i64::from(b.max.x) >= k_large_val
            || i64::from(b.max.y) >= k_large_val
    };
    let corners = |b: &Box2i| -> [Arg<'static>; 4] {
        [
            b.min.x.into(),
            b.min.y.into(),
            b.max.x.into(),
            b.max.y.into(),
        ]
    };
    if bad(&dspw) {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid display window (%d, %d - %d, %d)",
            &corners(&dspw),
        ));
    }
    if bad(&dw) {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid data window (%d, %d - %d, %d)",
            &corners(&dw),
        ));
    }
    if maxw > 0 && i64::from(maxw) < w {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid width (%lld) too large (max %d)",
            &[Arg::I64(w), maxw.into()],
        ));
    }
    if maxh > 0 && i64::from(maxh) < h {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid height (%lld) too large (max %d)",
            &[Arg::I64(h), maxh.into()],
        ));
    }
    if maxw > 0 && maxh > 0 {
        let max_num = i64::from(maxw) * i64::from(maxh);
        let ccount = if curpart.chunk_count_attr.is_some() {
            i64::from(curpart.chunk_count)
        } else {
            0
        };
        if ccount > max_num {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "Invalid chunkCount (%lld) exceeds maximum area of %lld",
                &[Arg::I64(ccount), Arg::I64(max_num)],
            ));
        }
    }
    // isnormal will return true when par is 0, which should also be disallowed
    if !par.is_normal() || par < 1e-6 || par > 1e6 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid pixel aspect ratio %g",
            &[Arg::F64(f64::from(par))],
        ));
    }
    if sww < 0.0 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid screen window width %g",
            &[Arg::F64(f64::from(sww))],
        ));
    }
    Ok(())
}

/// Port of `validate_channels` (src/lib/OpenEXRCore/validation.c:386-478 @ v3.5.3).
fn validate_channels(ctxt: &Context, curpart: &Part) -> Result<(), Error> {
    let channels = match value(curpart, curpart.channels) {
        None => {
            return Err(ctxt.print_error(
                ErrorCode::MissingReqAttr,
                "'channels' attribute not found",
                &[],
            ));
        }
        Some(AttrValue::Chlist(c)) => c,
        Some(_) => {
            return Err(ctxt.print_error(
                ErrorCode::AttrTypeMismatch,
                "'channels' attribute has wrong data type, expect chlist",
                &[],
            ));
        }
    };
    if curpart.data_window_attr.is_none() {
        return Err(ctxt.report_error(
            ErrorCode::NoAttrByName,
            b"request to validate channel list, but data window not set to validate against",
        ));
    }
    if channels.num_channels <= 0 {
        return Err(ctxt.report_error(ErrorCode::FileBadHeader, b"At least one channel required"));
    }
    // Internal coding pipeline structures store the channel count in an int16_t: reject
    // larger lists outright.
    if channels.num_channels > i32::from(i16::MAX) {
        return Err(ctxt.print_error(
            ErrorCode::FileBadHeader,
            "Channel count (%d) exceeds maximum supported value (%d)",
            &[channels.num_channels.into(), i32::from(i16::MAX).into()],
        ));
    }
    let dw = curpart.data_window;
    let w = i64::from(dw.max.x) - i64::from(dw.min.x) + 1;
    let h = i64::from(dw.max.y) - i64::from(dw.min.y) + 1;
    for e in &channels.entries {
        let name = Arg::Str(e.name.c_str());
        let xsamp = e.x_sampling;
        let ysamp = e.y_sampling;
        if xsamp < 1 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': x subsampling factor is invalid (%d)",
                &[name, xsamp.into()],
            ));
        }
        if ysamp < 1 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': y subsampling factor is invalid (%d)",
                &[name, ysamp.into()],
            ));
        }
        if dw.min.x % xsamp != 0 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': minimum x coordinate (%d) of the data window is not a multiple of the x subsampling factor (%d)",
                &[name, dw.min.x.into(), xsamp.into()],
            ));
        }
        if dw.min.y % ysamp != 0 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': minimum y coordinate (%d) of the data window is not a multiple of the y subsampling factor (%d)",
                &[name, dw.min.y.into(), ysamp.into()],
            ));
        }
        if w % i64::from(xsamp) != 0 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': row width (%lld) of the data window is not a multiple of the x subsampling factor (%d)",
                &[name, Arg::I64(w), xsamp.into()],
            ));
        }
        if h % i64::from(ysamp) != 0 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': column height (%lld) of the data window is not a multiple of the y subsampling factor (%d)",
                &[name, Arg::I64(h), ysamp.into()],
            ));
        }
    }
    Ok(())
}

/// Port of `validate_part_type` (src/lib/OpenEXRCore/validation.c:482-559 @ v3.5.3).
fn validate_part_type(ctxt: &mut Context, p: usize) -> Result<(), Error> {
    let num_parts = ctxt.parts.len();
    let is_singlepart_tiled = ctxt.is_singlepart_tiled;
    let is_multipart = ctxt.is_multipart;
    let strict_header = ctxt.strict_header;
    let Context { core, parts, .. } = ctxt;
    let core: &ContextCore = core;
    let curpart = &mut parts[p];
    if let Some(type_id) = curpart.type_ {
        // see if the type overwrote the storage mode
        if is_singlepart_tiled {
            if is_multipart || num_parts > 1 {
                return Err(core.print_error(
                    ErrorCode::InvalidAttr,
                    "Multipart files cannot have the tiled bit set",
                    &[],
                ));
            }
            if curpart.storage_mode != Storage::TILED {
                curpart.storage_mode = Storage::TILED;
                if strict_header {
                    return Err(core.print_error(
                        ErrorCode::InvalidAttr,
                        "attribute 'type': Single part tiled flag set but not marked as tiled storage type",
                        &[],
                    ));
                }
            }
        }
        let expectedtype: Option<&str> = match curpart.storage_mode {
            Storage::SCANLINE => Some("scanlineimage"),
            Storage::TILED => Some("tiledimage"),
            Storage::DEEP_SCANLINE => Some("deepscanline"),
            Storage::DEEP_TILED => Some("deeptile"),
            _ => None,
        };
        if let Some(expected) = expectedtype {
            let attr = curpart.attributes.get_mut(type_id).expect("type attribute");
            let AttrValue::String(s) = &mut attr.value else {
                unreachable!("validate_req_attr checked the type")
            };
            if s.c_str() != expected.as_bytes() {
                if core.mode == ContextMode::Write {
                    return Err(Error::bare(ErrorCode::InvalidAttr));
                }
                if strict_header {
                    return Err(core.print_error(
                        ErrorCode::InvalidAttr,
                        "attribute 'type': Type should be '%s' but set to '%s', believing file flags",
                        &[expected.into(), Arg::Str(s.c_str())],
                    ));
                }
                // C++ silently changed this
                if core.attr_string_set(s, Some(expected.as_bytes())).is_err() {
                    return Err(core.print_error(
                        ErrorCode::InvalidAttr,
                        "attribute 'type': Mismatch between file flags and type attribute, unable to fix",
                        &[],
                    ));
                }
            }
        }
    }
    // NB: we allow an 'unknown' storage type of EXR_STORAGE_UNKNOWN for future proofing
    if curpart.storage_mode == Storage::LAST_TYPE {
        return Err(core.print_error(
            ErrorCode::InvalidAttr,
            "Unable to determine data storage type for part",
            &[],
        ));
    }
    Ok(())
}

/// Port of `validate_tile_data` (src/lib/OpenEXRCore/validation.c:563-666 @ v3.5.3).
fn validate_tile_data(ctxt: &Context, curpart: &Part) -> Result<(), Error> {
    if curpart.storage_mode != Storage::TILED && curpart.storage_mode != Storage::DEEP_TILED {
        return Ok(());
    }
    let maxtilew = ctxt.max_tile_w;
    let maxtileh = ctxt.max_tile_h;
    let desc = match value(curpart, curpart.tiles) {
        None => {
            return Err(ctxt.print_error(
                ErrorCode::MissingReqAttr,
                "'tiles' attribute for tiled file not found",
                &[],
            ));
        }
        Some(AttrValue::TileDesc(d)) => *d,
        Some(_) => {
            return Err(ctxt.print_error(
                ErrorCode::AttrTypeMismatch,
                "'tiles' attribute has wrong data type, expect tile description",
                &[],
            ));
        }
    };
    let (xs, ys) = (desc.x_size, desc.y_size);
    let levmode = desc.level_mode();
    let rndmode = desc.round_mode();
    if xs == 0 || ys == 0 || xs > (i32::MAX / 4) as u32 || ys > (i32::MAX / 4) as u32 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid tile description size (%u x %u)",
            &[xs.into(), ys.into()],
        ));
    }
    if maxtilew > 0 && maxtilew < xs as i32 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Width of tile exceeds max size (%d vs max %d)",
            &[(xs as i32).into(), maxtilew.into()],
        ));
    }
    if maxtileh > 0 && maxtileh < ys as i32 {
        // upstream reports the height with the width's message
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Width of tile exceeds max size (%d vs max %d)",
            &[(ys as i32).into(), maxtileh.into()],
        ));
    }
    if levmode.0 < TileLevelMode::ONE_LEVEL.0 || levmode.0 >= TileLevelMode::LAST_TYPE.0 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid level mode (%d) in tile description header",
            &[levmode.0.into()],
        ));
    }
    if rndmode.0 < TileRoundMode::ROUND_DOWN.0 || rndmode.0 >= TileRoundMode::LAST_TYPE.0 {
        return Err(ctxt.print_error(
            ErrorCode::InvalidAttr,
            "Invalid rounding mode (%d) in tile description header",
            &[rndmode.0.into()],
        ));
    }
    check_unit_sampling(ctxt, curpart, "a tiled image")
}

/// The per-channel sampling check `validate_tile_data` and `validate_deep_data` share.
fn check_unit_sampling(ctxt: &Context, curpart: &Part, what: &str) -> Result<(), Error> {
    let Some(AttrValue::Chlist(channels)) = value(curpart, curpart.channels) else {
        return Ok(());
    };
    for e in &channels.entries {
        if e.x_sampling != 1 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': x subsampling factor is not 1 (%d) for %s",
                &[Arg::Str(e.name.c_str()), e.x_sampling.into(), what.into()],
            ));
        }
        if e.y_sampling != 1 {
            return Err(ctxt.print_error(
                ErrorCode::InvalidAttr,
                "channel '%s': y subsampling factor is not 1 (%d) for %s",
                &[Arg::Str(e.name.c_str()), e.y_sampling.into(), what.into()],
            ));
        }
    }
    Ok(())
}

/// Port of `validate_deep_data` (src/lib/OpenEXRCore/validation.c:670-703 @ v3.5.3).
fn validate_deep_data(ctxt: &Context, curpart: &Part) -> Result<(), Error> {
    if curpart.storage_mode != Storage::DEEP_SCANLINE && curpart.storage_mode != Storage::DEEP_TILED
    {
        return Ok(());
    }
    if !compression_is_valid_for_deep(curpart.comp_type) {
        return Err(ctxt.report_error(ErrorCode::InvalidAttr, b"Invalid compression for deep data"));
    }
    check_unit_sampling(ctxt, curpart, "a deep image")
}

/// Port of `internal_exr_validate_read_part` (src/lib/OpenEXRCore/validation.c:707-729
/// @ v3.5.3): defaults are added for missing attributes unless the header is strict.
pub(crate) fn internal_exr_validate_read_part(ctxt: &mut Context, p: usize) -> Result<(), Error> {
    let strict = ctxt.strict_header;
    validate_req_attr(ctxt, p, !strict)?;
    validate_image_dimensions(ctxt, &ctxt.parts[p])?;
    validate_channels(ctxt, &ctxt.parts[p])?;
    validate_part_type(ctxt, p)?;
    validate_tile_data(ctxt, &ctxt.parts[p])?;
    validate_deep_data(ctxt, &ctxt.parts[p])?;
    Ok(())
}

/// Bitwise equality of two values of the same attribute type, as upstream's `memcmp`s.
fn same_bits(a: &AttrValue, b: &AttrValue) -> bool {
    match (a, b) {
        (AttrValue::Box2i(x), AttrValue::Box2i(y)) => x == y,
        (AttrValue::Float(x), AttrValue::Float(y)) => x.to_bits() == y.to_bits(),
        (AttrValue::Timecode(x), AttrValue::Timecode(y)) => x == y,
        (AttrValue::Chromaticities(x), AttrValue::Chromaticities(y)) => {
            let bits = |c: &Chromaticities| {
                [
                    c.red_x, c.red_y, c.green_x, c.green_y, c.blue_x, c.blue_y, c.white_x,
                    c.white_y,
                ]
                .map(f32::to_bits)
            };
            bits(x) == bits(y)
        }
        _ => false,
    }
}

/// `exr_get_attribute_by_name` on a part, silent when the attribute is missing.
fn by_name<'p>(ctxt: &Context, part: &'p Part, name: &str) -> Option<&'p AttrValue> {
    ctxt.attr_list_find_by_name(&part.attributes, Some(name.as_bytes()))
        .ok()
        .and_then(|id| part.attributes.get(id))
        .map(|a| &a.value)
}

/// Port of `internal_exr_validate_shared_attrs` (src/lib/OpenEXRCore/validation.c:733-866
/// @ v3.5.3): the names of the attributes that part `curpartidx` must share with part 0 but
/// does not (displayWindow, pixelAspectRatio, timecode, chromaticities).
pub(crate) fn internal_exr_validate_shared_attrs(
    ctxt: &Context,
    curpartidx: usize,
) -> Result<(), Vec<&'static str>> {
    let basepart = &ctxt.parts[0];
    let curpart = &ctxt.parts[curpartidx];
    let mut mismatch: Vec<&'static str> = Vec::new();
    // A slot both parts may hold: equal when both are absent, or both of `t` with equal bits.
    let differs = |b: Option<&AttrValue>, c: Option<&AttrValue>, t: AttributeType| match (b, c) {
        (Some(b), Some(c)) => {
            b.attr_type() != t || b.attr_type() != c.attr_type() || !same_bits(b, c)
        }
        (None, None) => false,
        _ => true,
    };
    if differs(
        value(basepart, basepart.display_window_attr),
        value(curpart, curpart.display_window_attr),
        AttributeType::BOX2I,
    ) {
        mismatch.push("displayWindow");
    }
    if differs(
        value(basepart, basepart.pixel_aspect_ratio),
        value(curpart, curpart.pixel_aspect_ratio),
        AttributeType::FLOAT,
    ) {
        mismatch.push("pixelAspectRatio");
    }
    // Looked up by name: a part without the attribute matches whatever part 0 has (upstream's
    // `rv == rv1` test), but one with it must match part 0's.
    for (name, t) in [
        ("timecode", AttributeType::TIMECODE),
        ("chromaticities", AttributeType::CHROMATICITIES),
    ] {
        let bad = match (by_name(ctxt, basepart, name), by_name(ctxt, curpart, name)) {
            (Some(b), Some(c)) => differs(Some(b), Some(c), t),
            (None, Some(_)) => true,
            _ => false,
        };
        if bad {
            mismatch.push(name);
        }
    }
    if mismatch.is_empty() {
        Ok(())
    } else {
        Err(mismatch)
    }
}
