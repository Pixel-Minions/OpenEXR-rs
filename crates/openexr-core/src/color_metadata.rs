// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `color_metadata.c` (src/lib/OpenEXRCore/color_metadata.c:1-387 @ v3.5.3) and
//! `openexr_color_metadata.h` (src/lib/OpenEXRCore/openexr_color_metadata.h:1-224 @ v3.5.3):
//! the Color Interop IDs and their chromaticities, and the checks of a part's color metadata.
//!

use crate::attributes::AttrValue;
use crate::context::{Context, Error};
use crate::errors::{ErrorCode, ExrResult};
use crate::openexr_attr::Chromaticities;

/// `EXR_COLOR_INTEROP_ID_CHROMATICITIES_TOLERANCE`
/// (src/lib/OpenEXRCore/openexr_color_metadata.h:47 @ v3.5.3).
pub const EXR_COLOR_INTEROP_ID_CHROMATICITIES_TOLERANCE: f32 = 0.001;

/// Port of `exr_color_metadata_warning_t` (src/lib/OpenEXRCore/openexr_color_metadata.h:92-140
/// @ v3.5.3): bit flags.
pub mod warnings {
    pub const EXR_COLOR_METADATA_OK: u32 = 0;
    pub const EXR_COLOR_METADATA_EMPTY_INTEROP_ID: u32 = 1 << 0;
    pub const EXR_COLOR_METADATA_CHROMATICITIES_DIFFER: u32 = 1 << 1;
    pub const EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES: u32 = 1 << 2;
    pub const EXR_COLOR_METADATA_DATA_HAS_WHITE_LUMINANCE: u32 = 1 << 3;
    pub const EXR_COLOR_METADATA_DATA_HAS_ADOPTED_NEUTRAL: u32 = 1 << 4;
    pub const EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED: u32 = 1 << 5;
    pub const EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE: u32 = 1 << 6;
    pub const EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0: u32 = 1 << 7;
    pub const EXR_COLOR_METADATA_ACES_FLAG_NO_CHROMATICITIES: u32 = 1 << 8;
    pub const EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0: u32 = 1 << 9;
}
use warnings::*;

/// Port of `exr_color_metadata_t` (src/lib/OpenEXRCore/openexr_color_metadata.h:142-196
/// @ v3.5.3). Strings are C strings: `None` for NULL, their bytes up to the NUL otherwise.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColorMetadata<'a> {
    pub color_interop_id: Option<&'a [u8]>,
    pub chromaticities: Option<&'a Chromaticities>,
    pub has_white_luminance: i32,
    pub has_adopted_neutral: i32,
    pub has_aces_image_container_flag: i32,
    pub aces_image_container_flag: i32,
    pub first_part_color_interop_id: Option<&'a [u8]>,
    pub compare_to_first_part: i32,
}

const fn chroma(r: (f32, f32), g: (f32, f32), b: (f32, f32), w: (f32, f32)) -> Chromaticities {
    Chromaticities {
        red_x: r.0,
        red_y: r.1,
        green_x: g.0,
        green_y: g.1,
        blue_x: b.0,
        blue_y: b.1,
        white_x: w.0,
        white_y: w.1,
    }
}

/// `D65_WHITE` (src/lib/OpenEXRCore/color_metadata.c:40 @ v3.5.3).
const D65_WHITE: (f32, f32) = (0.3127, 0.3290);
/// `ACES_WHITE` (src/lib/OpenEXRCore/color_metadata.c:41 @ v3.5.3).
const ACES_WHITE: (f32, f32) = (0.32168, 0.33767);

/// `the_color_interop_id_chromaticities` (src/lib/OpenEXRCore/color_metadata.c:45-53 @ v3.5.3),
/// in upstream's order, which is the order a match is reported in.
static THE_COLOR_INTEROP_ID_CHROMATICITIES: [(&str, Chromaticities); 6] = [
    (
        "lin_rec709_scene",
        chroma(
            (0.6400, 0.3300),
            (0.3000, 0.6000),
            (0.1500, 0.0600),
            D65_WHITE,
        ),
    ),
    (
        "lin_ap0_scene",
        chroma(
            (0.7347, 0.2653),
            (0.0000, 1.0000),
            (0.0001, -0.0770),
            ACES_WHITE,
        ),
    ),
    (
        "lin_ap1_scene",
        chroma(
            (0.7130, 0.2930),
            (0.1650, 0.8300),
            (0.1280, 0.0440),
            ACES_WHITE,
        ),
    ),
    (
        "lin_p3d65_scene",
        chroma(
            (0.6800, 0.3200),
            (0.2650, 0.6900),
            (0.1500, 0.0600),
            D65_WHITE,
        ),
    ),
    (
        "lin_rec2020_scene",
        chroma(
            (0.7080, 0.2920),
            (0.1700, 0.7970),
            (0.1310, 0.0460),
            D65_WHITE,
        ),
    ),
    (
        "lin_adobergb_scene",
        chroma(
            (0.6400, 0.3300),
            (0.2100, 0.7100),
            (0.1500, 0.0600),
            D65_WHITE,
        ),
    ),
];

/// The bytes of a C string up to its NUL.
fn c_str(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// Port of `exr_color_interop_id_to_chromaticities` (src/lib/OpenEXRCore/color_metadata.c:66-85
/// @ v3.5.3).
pub fn color_interop_id_to_chromaticities(
    color_interop_id: Option<&[u8]>,
) -> Result<Chromaticities, ExrResult> {
    let Some(id) = color_interop_id else {
        return Err(ErrorCode::InvalidArgument.code());
    };
    let id = c_str(id);
    THE_COLOR_INTEROP_ID_CHROMATICITIES
        .iter()
        .find(|(n, _)| n.as_bytes() == id)
        .map(|(_, c)| *c)
        .ok_or(ErrorCode::NoAttrByName.code())
}

/// Port of `within_tolerance` (src/lib/OpenEXRCore/color_metadata.c:88-92 @ v3.5.3).
fn within_tolerance(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

/// Port of `exr_chromaticities_to_color_interop_id` (src/lib/OpenEXRCore/color_metadata.c:94-124
/// @ v3.5.3): the first ID whose chromaticities are all within `tolerance`.
pub fn chromaticities_to_color_interop_id(
    chromaticities: Option<&Chromaticities>,
    tolerance: f32,
) -> Result<&'static str, ExrResult> {
    let Some(c) = chromaticities else {
        return Err(ErrorCode::InvalidArgument.code());
    };
    // !(tolerance >= 0.f): NaN is refused too
    if !(tolerance >= 0.0) {
        return Err(ErrorCode::InvalidArgument.code());
    }
    for (id, e) in &THE_COLOR_INTEROP_ID_CHROMATICITIES {
        if within_tolerance(e.red_x, c.red_x, tolerance)
            && within_tolerance(e.red_y, c.red_y, tolerance)
            && within_tolerance(e.green_x, c.green_x, tolerance)
            && within_tolerance(e.green_y, c.green_y, tolerance)
            && within_tolerance(e.blue_x, c.blue_x, tolerance)
            && within_tolerance(e.blue_y, c.blue_y, tolerance)
            && within_tolerance(e.white_x, c.white_x, tolerance)
            && within_tolerance(e.white_y, c.white_y, tolerance)
        {
            return Ok(id);
        }
    }
    Err(ErrorCode::NoAttrByName.code())
}

/// Port of `exr_check_color_metadata_values` (src/lib/OpenEXRCore/color_metadata.c:127-271
/// @ v3.5.3): the warnings for one part's color metadata.
pub fn check_color_metadata_values(values: &ColorMetadata<'_>) -> u32 {
    let mut found = EXR_COLOR_METADATA_OK;
    let id = values.color_interop_id.map(c_str);
    if let Some(id) = id {
        // An empty ID says nothing; the attribute should be omitted, or set to "unknown".
        if id.is_empty() {
            found |= EXR_COLOR_METADATA_EMPTY_INTEROP_ID;
        }
        // If the ID names a color space with defined chromaticities, and the part also
        // carries chromaticities, the two should agree.
        if values.chromaticities.is_some() && color_interop_id_to_chromaticities(Some(id)).is_ok() {
            match chromaticities_to_color_interop_id(
                values.chromaticities,
                EXR_COLOR_INTEROP_ID_CHROMATICITIES_TOLERANCE,
            ) {
                Ok(from) if from.as_bytes() == id => {}
                _ => found |= EXR_COLOR_METADATA_CHROMATICITIES_DIFFER,
            }
        }
        if id == b"data" {
            // A part tagged "data" is deliberately not color managed.
            if values.chromaticities.is_some() {
                found |= EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES;
            }
            if values.has_white_luminance != 0 {
                found |= EXR_COLOR_METADATA_DATA_HAS_WHITE_LUMINANCE;
            }
            if values.has_adopted_neutral != 0 {
                found |= EXR_COLOR_METADATA_DATA_HAS_ADOPTED_NEUTRAL;
            }
        } else if values.compare_to_first_part != 0 {
            match values.first_part_color_interop_id.map(c_str) {
                Some(first) if first == id => {}
                _ => found |= EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED,
            }
        }
    }

    // acesImageContainerFlag asserts compliance with SMPTE ST 2065-4: the data must be
    // ACES2065-1, colorInteropID lin_ap0_scene, with chromaticities.
    if values.has_aces_image_container_flag != 0 {
        if values.aces_image_container_flag != 1 {
            found |= EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE;
        }
        if id.is_some_and(|id| id != b"lin_ap0_scene") {
            found |= EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0;
        }
        if values.chromaticities.is_none() {
            found |= EXR_COLOR_METADATA_ACES_FLAG_NO_CHROMATICITIES;
        } else {
            match chromaticities_to_color_interop_id(
                values.chromaticities,
                EXR_COLOR_INTEROP_ID_CHROMATICITIES_TOLERANCE,
            ) {
                Ok("lin_ap0_scene") => {}
                _ => found |= EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0,
            }
        }
    }
    found
}

/// Port of `exr_get_color_metadata_warning_as_string`
/// (src/lib/OpenEXRCore/color_metadata.c:357-387 @ v3.5.3).
pub fn get_color_metadata_warning_as_string(warning: u32) -> &'static str {
    match warning {
        EXR_COLOR_METADATA_OK => "EXR_COLOR_METADATA_OK",
        EXR_COLOR_METADATA_EMPTY_INTEROP_ID => "colorInteropID is empty",
        EXR_COLOR_METADATA_CHROMATICITIES_DIFFER => {
            "the chromaticities are not those denoted by the colorInteropID"
        }
        EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES => {
            "colorInteropID is 'data' but the part has a chromaticities attribute"
        }
        EXR_COLOR_METADATA_DATA_HAS_WHITE_LUMINANCE => {
            "colorInteropID is 'data' but the part has a whiteLuminance attribute"
        }
        EXR_COLOR_METADATA_DATA_HAS_ADOPTED_NEUTRAL => {
            "colorInteropID is 'data' but the part has an adoptedNeutral attribute"
        }
        EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED => {
            "colorInteropID does not match the first part's"
        }
        EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE => {
            "acesImageContainerFlag is present but is not an int of value 1"
        }
        EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0 => {
            "acesImageContainerFlag is present but colorInteropID is not 'lin_ap0_scene'"
        }
        EXR_COLOR_METADATA_ACES_FLAG_NO_CHROMATICITIES => {
            "acesImageContainerFlag is present but the part has no chromaticities attribute"
        }
        EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0 => {
            "acesImageContainerFlag is present but the chromaticities are not those of 'lin_ap0_scene'"
        }
        _ => "Unknown color metadata warning",
    }
}

/// Port of `get_color_interop_id` (src/lib/OpenEXRCore/color_metadata.c:278-290 @ v3.5.3): the
/// part's colorInteropID, or `None` when it has none or it is not a string.
fn get_color_interop_id(ctxt: &Context, part_index: i32) -> Option<&[u8]> {
    match ctxt.get_attribute_by_name(part_index, Some("colorInteropID")) {
        Ok(a) => match &a.value {
            AttrValue::String(s) => Some(s.c_str()),
            _ => None,
        },
        Err(_) => None,
    }
}

/// Port of `has_attribute` (src/lib/OpenEXRCore/color_metadata.c:292-299 @ v3.5.3).
fn has_attribute(ctxt: &Context, part_index: i32, name: &str) -> bool {
    ctxt.get_attribute_by_name(part_index, Some(name)).is_ok()
}

impl Context {
    /// Port of `exr_check_color_metadata` (src/lib/OpenEXRCore/color_metadata.c:301-354
    /// @ v3.5.3): the warnings for part `part_index`'s color metadata.
    pub fn check_color_metadata(&self, part_index: i32) -> Result<u32, Error> {
        let num_parts = self.get_count();
        if part_index < 0 || part_index >= num_parts {
            return Err(Error::bare(ErrorCode::IncorrectPart));
        }
        let chromaticities = self
            .attr_get_chromaticities(part_index, Some("chromaticities"))
            .ok();
        let mut values = ColorMetadata {
            color_interop_id: get_color_interop_id(self, part_index),
            chromaticities: chromaticities.as_ref(),
            has_white_luminance: i32::from(has_attribute(self, part_index, "whiteLuminance")),
            has_adopted_neutral: i32::from(has_attribute(self, part_index, "adoptedNeutral")),
            ..Default::default()
        };
        if let Ok(attr) = self.get_attribute_by_name(part_index, Some("acesImageContainerFlag")) {
            values.has_aces_image_container_flag = 1;
            // A present attribute of the wrong type is reported the same way as a wrong value,
            // so any non-conforming value will do here.
            values.aces_image_container_flag = match &attr.value {
                AttrValue::Int(i) => *i,
                _ => 0,
            };
        }
        if part_index > 0 {
            values.compare_to_first_part = 1;
            values.first_part_color_interop_id = get_color_interop_id(self, 0);
        }
        Ok(check_color_metadata_values(&values))
    }
}
