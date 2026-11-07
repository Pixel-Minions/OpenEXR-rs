// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/color_metadata.cpp` @ v3.5.3.

use crate::color_metadata::warnings::*;
use crate::color_metadata::*;
use crate::errors::ErrorCode;
use crate::openexr_attr::Chromaticities;

/// `allIDs` (src/test/OpenEXRCoreTest/color_metadata.cpp:19-25 @ v3.5.3).
const ALL_IDS: [&str; 6] = [
    "lin_rec709_scene",
    "lin_ap0_scene",
    "lin_ap1_scene",
    "lin_p3d65_scene",
    "lin_rec2020_scene",
    "lin_adobergb_scene",
];

/// `memcmp` of two chromaticities structs: their bits.
fn bits(c: &Chromaticities) -> [u32; 8] {
    [
        c.red_x, c.red_y, c.green_x, c.green_y, c.blue_x, c.blue_y, c.white_x, c.white_y,
    ]
    .map(f32::to_bits)
}

fn to_chroma(id: &str) -> Result<Chromaticities, crate::errors::ExrResult> {
    color_interop_id_to_chromaticities(Some(id.as_bytes()))
}

const TOL: f32 = EXR_COLOR_INTEROP_ID_CHROMATICITIES_TOLERANCE;

/// Port of `testColorInteropChromaticities`
/// (src/test/OpenEXRCoreTest/color_metadata.cpp:146-306) @ v3.5.3. The calls with a NULL output
/// pointer have no Rust counterpart (PLAN.md O8).
#[test]
fn test_color_interop_chromaticities() {
    // Every supported ID round trips, and the six entries are distinct.
    for (i, idi) in ALL_IDS.iter().enumerate() {
        let chroma = to_chroma(idi).unwrap();
        let id = chromaticities_to_color_interop_id(Some(&chroma), TOL).unwrap();
        assert_eq!(id, *idi);
        for idj in &ALL_IDS[..i] {
            let other = to_chroma(idj).unwrap();
            assert_ne!(bits(&other), bits(&chroma));
        }
    }

    // Spot check the table values, including the negative blue y of lin_ap0_scene.
    let chroma = to_chroma("lin_ap0_scene").unwrap();
    assert_eq!(chroma.red_x, 0.73470f32);
    assert_eq!(chroma.red_y, 0.26530f32);
    assert_eq!(chroma.green_x, 0.00000f32);
    assert_eq!(chroma.green_y, 1.00000f32);
    assert_eq!(chroma.blue_x, 0.00010f32);
    assert_eq!(chroma.blue_y, -0.07700f32);
    assert_eq!(chroma.white_x, 0.32168f32);
    assert_eq!(chroma.white_y, 0.33767f32);

    // IDs with no defined mapping, including the two reserved values (a failed lookup
    // returns no value, so it leaves nothing changed).
    let no_attr = ErrorCode::NoAttrByName;
    assert_eq!(to_chroma("unknown").unwrap_err(), no_attr);
    assert_eq!(to_chroma("data").unwrap_err(), no_attr);
    assert_eq!(to_chroma("").unwrap_err(), no_attr);
    assert_eq!(to_chroma("ocio:lin_awg4_scene").unwrap_err(), no_attr);

    // Chromaticities matching no entry.
    let nonsense = Chromaticities {
        red_x: 0.1,
        red_y: 0.2,
        green_x: 0.3,
        green_y: 0.4,
        blue_x: 0.5,
        blue_y: 0.6,
        white_x: 0.7,
        white_y: 0.8,
    };
    assert_eq!(
        chromaticities_to_color_interop_id(Some(&nonsense), TOL).unwrap_err(),
        no_attr
    );

    // Matching is within +/- tolerance in x and y, on every coordinate.
    let offset: f32 = 0.0009;
    let mut nudged = to_chroma("lin_p3d65_scene").unwrap();
    nudged.red_x += offset;
    nudged.green_y -= offset;
    nudged.white_x += offset;
    let id = chromaticities_to_color_interop_id(Some(&nudged), TOL).unwrap();
    assert_eq!(id, "lin_p3d65_scene");
    // and just outside it does not match at all
    nudged.red_x += 0.01;
    assert_eq!(
        chromaticities_to_color_interop_id(Some(&nudged), TOL).unwrap_err(),
        no_attr
    );

    // lin_rec709_scene and lin_adobergb_scene differ only in green: a loose tolerance matches
    // whichever comes first in the table.
    let adobergb = to_chroma("lin_adobergb_scene").unwrap();
    let id = chromaticities_to_color_interop_id(Some(&adobergb), 0.2).unwrap();
    assert_eq!(id, "lin_rec709_scene");

    // Bad arguments.
    let inval = ErrorCode::InvalidArgument;
    assert_eq!(color_interop_id_to_chromaticities(None).unwrap_err(), inval);
    assert_eq!(
        chromaticities_to_color_interop_id(None, 0.001).unwrap_err(),
        inval
    );
    assert_eq!(
        chromaticities_to_color_interop_id(Some(&chroma), -0.001).unwrap_err(),
        inval
    );
}

/// Port of `testColorMetadataValues` (src/test/OpenEXRCoreTest/color_metadata.cpp:308-630)
/// @ v3.5.3. The calls with a NULL argument have no Rust counterpart (PLAN.md O8).
#[test]
fn test_color_metadata_values() {
    let ap1 = to_chroma("lin_ap1_scene").unwrap();
    let rec709 = to_chroma("lin_rec709_scene").unwrap();
    let nonsense = Chromaticities {
        red_x: 0.1,
        red_y: 0.2,
        green_x: 0.3,
        green_y: 0.4,
        blue_x: 0.5,
        blue_y: 0.6,
        white_x: 0.7,
        white_y: 0.8,
    };
    let id = |s: &'static str| Some(s.as_bytes());
    let check = |v: &ColorMetadata<'_>| check_color_metadata_values(v);

    // With no colorInteropID, the chromaticities are not checked.
    let values = ColorMetadata {
        chromaticities: Some(&nonsense),
        has_white_luminance: 1,
        has_adopted_neutral: 1,
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);

    // An ID that agrees with the chromaticities is clean.
    let values = ColorMetadata {
        color_interop_id: id("lin_ap1_scene"),
        chromaticities: Some(&ap1),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);

    // An ID with no chromaticities mapping is not checked against them.
    let values = ColorMetadata {
        color_interop_id: id("ocio:lin_awg4_scene"),
        chromaticities: Some(&rec709),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);

    // An ID that disagrees with the chromaticities is reported.
    let values = ColorMetadata {
        color_interop_id: id("lin_ap1_scene"),
        chromaticities: Some(&rec709),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_CHROMATICITIES_DIFFER);

    // Chromaticities that match no known color space at all are the same finding.
    let values = ColorMetadata {
        color_interop_id: id("lin_ap1_scene"),
        chromaticities: Some(&nonsense),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_CHROMATICITIES_DIFFER);

    // An empty ID says nothing.
    let values = ColorMetadata {
        color_interop_id: id(""),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_EMPTY_INTEROP_ID);

    // A part tagged "data": each colorimetry attribute on it is reported, and they combine.
    let values = ColorMetadata {
        color_interop_id: id("data"),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);
    let values = ColorMetadata {
        color_interop_id: id("data"),
        chromaticities: Some(&ap1),
        has_white_luminance: 1,
        has_adopted_neutral: 1,
        ..Default::default()
    };
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES
            | EXR_COLOR_METADATA_DATA_HAS_WHITE_LUMINANCE
            | EXR_COLOR_METADATA_DATA_HAS_ADOPTED_NEUTRAL
    );

    // A later part may omit colorInteropID, set it to "data", or repeat it.
    let mut values = ColorMetadata {
        compare_to_first_part: 1,
        first_part_color_interop_id: id("lin_ap1_scene"),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);
    values.color_interop_id = id("data");
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);
    values.color_interop_id = id("lin_ap1_scene");
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);

    // But any other value that differs from the first part's is ambiguous, as is a first
    // part with no ID.
    values.color_interop_id = id("lin_ap0_scene");
    assert_eq!(check(&values), EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED);
    values.first_part_color_interop_id = None;
    assert_eq!(check(&values), EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED);

    // The shared attribute rule is only applied when asked for.
    values.compare_to_first_part = 0;
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);

    // Per-part and shared problems are reported together.
    let values = ColorMetadata {
        color_interop_id: id("lin_ap0_scene"),
        chromaticities: Some(&rec709),
        compare_to_first_part: 1,
        first_part_color_interop_id: id("lin_ap1_scene"),
        ..Default::default()
    };
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_CHROMATICITIES_DIFFER | EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED
    );

    // acesImageContainerFlag asserts ST 2065-4 compliance.
    let ap0 = to_chroma("lin_ap0_scene").unwrap();
    let mut values = ColorMetadata {
        has_aces_image_container_flag: 1,
        aces_image_container_flag: 1,
        color_interop_id: id("lin_ap0_scene"),
        chromaticities: Some(&ap0),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);
    // The colorInteropID may be omitted; the chromaticities may not.
    values.color_interop_id = None;
    assert_eq!(check(&values), EXR_COLOR_METADATA_OK);
    values.chromaticities = None;
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_ACES_FLAG_NO_CHROMATICITIES
    );
    // Chromaticities of some other color space, or of none.
    values.chromaticities = Some(&rec709);
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0
    );
    values.chromaticities = Some(&nonsense);
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0
    );

    // A present colorInteropID has to agree; here only the ACES rule is broken, twice.
    let values = ColorMetadata {
        has_aces_image_container_flag: 1,
        aces_image_container_flag: 1,
        color_interop_id: id("lin_rec709_scene"),
        chromaticities: Some(&rec709),
        ..Default::default()
    };
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0
            | EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0
    );

    // 1 is the only defined value of the flag.
    let mut values = ColorMetadata {
        has_aces_image_container_flag: 1,
        aces_image_container_flag: 0,
        color_interop_id: id("lin_ap0_scene"),
        chromaticities: Some(&ap0),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE);
    values.aces_image_container_flag = 2;
    assert_eq!(check(&values), EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE);

    // Without the flag, none of this is checked.
    let values = ColorMetadata {
        color_interop_id: id("lin_rec709_scene"),
        chromaticities: Some(&ap0),
        ..Default::default()
    };
    assert_eq!(check(&values), EXR_COLOR_METADATA_CHROMATICITIES_DIFFER);

    // A part tagged "data" cannot also be an ACES container.
    let values = ColorMetadata {
        has_aces_image_container_flag: 1,
        aces_image_container_flag: 1,
        color_interop_id: id("data"),
        chromaticities: Some(&ap0),
        ..Default::default()
    };
    assert_eq!(
        check(&values),
        EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES
            | EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0
    );

    // Every flag has a distinct description.
    let all = [
        EXR_COLOR_METADATA_EMPTY_INTEROP_ID,
        EXR_COLOR_METADATA_CHROMATICITIES_DIFFER,
        EXR_COLOR_METADATA_DATA_HAS_CHROMATICITIES,
        EXR_COLOR_METADATA_DATA_HAS_WHITE_LUMINANCE,
        EXR_COLOR_METADATA_DATA_HAS_ADOPTED_NEUTRAL,
        EXR_COLOR_METADATA_INTEROP_ID_NOT_SHARED,
        EXR_COLOR_METADATA_ACES_FLAG_NOT_ONE,
        EXR_COLOR_METADATA_ACES_FLAG_INTEROP_ID_NOT_AP0,
        EXR_COLOR_METADATA_ACES_FLAG_NO_CHROMATICITIES,
        EXR_COLOR_METADATA_ACES_FLAG_CHROMATICITIES_NOT_AP0,
    ];
    for (i, w) in all.iter().enumerate() {
        let s = get_color_metadata_warning_as_string(*w);
        assert!(!s.is_empty());
        for wj in &all[..i] {
            assert_ne!(s, get_color_metadata_warning_as_string(*wj));
        }
    }
}
