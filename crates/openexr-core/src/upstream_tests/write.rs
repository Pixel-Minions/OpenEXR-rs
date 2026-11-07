// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/write.cpp` @ v3.5.3. Upstream's checks of NULL context
//! and output pointers are n/a (PLAN.md O8): a Rust call cannot pass them.

use std::sync::Arc;

use crate::coding::BufferRef;
use crate::context::{Context, DefaultWriteMode, ErrorReport, Initializer};
use crate::errors::ErrorCode;
use crate::openexr_attr::{
    Box2i, Compression, LineOrder, PerceptualTreatment, PixelType, Storage, TileLevelMode,
    TileRoundMode, V2f, V2i,
};
use crate::upstream_tests::DEFAULTS;
use crate::upstream_tests::read::{make_temp_dir, test_image_dir};

/// `err_cb` (src/test/OpenEXRCoreTest/write.cpp:30-35 @ v3.5.3).
fn err_cb(r: &ErrorReport<'_>) {
    eprintln!(
        "err_cb {} ({}): {}",
        crate::base::get_error_code_as_string(r.code),
        r.code,
        String::from_utf8_lossy(r.message)
    );
}

/// `cinit` with `cinit.error_handler_fn = &err_cb`.
fn cinit() -> Initializer {
    Initializer {
        error_handler: Some(Arc::new(err_cb)),
        ..Initializer::default()
    }
}

/// `exr_start_write (&outf, outfn, EXR_WRITE_FILE_DIRECTLY, &cinit)`.
fn start_write(outfn: &str) -> Context {
    Context::start_write(
        Some(outfn),
        DefaultWriteMode::WriteFileDirectly,
        Some(cinit()),
    )
    .expect("exr_start_write")
}

/// The code of a refused call.
fn code<T: std::fmt::Debug>(r: Result<T, crate::context::Error>) -> crate::errors::ExrResult {
    r.expect_err("the call is refused").code
}

/// Port of `testWriteBadArgs` (src/test/OpenEXRCoreTest/write.cpp:37-56) @ v3.5.3. Left out:
/// the NULL context pointer.
#[test]
fn test_write_bad_args() {
    let tempdir = make_temp_dir();
    let _fn = format!("{tempdir}invalid.exr");
    assert_eq!(
        code(Context::start_write(
            None,
            DefaultWriteMode::WriteFileDirectly,
            None
        )),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(Context::start_write(
            None,
            DefaultWriteMode::WriteFileDirectly,
            Some(cinit())
        )),
        ErrorCode::InvalidArgument
    );
}

/// Port of `testWriteBadFiles` (src/test/OpenEXRCoreTest/write.cpp:58-60) @ v3.5.3, which is
/// empty upstream.
#[test]
fn test_write_bad_files() {}

/// The checks of the name and storage of part 0 that testStartWriteScan, testStartWriteTile and
/// testWriteBaseHeader share, for a context of `count` parts.
fn check_part0(outf: &Context, count: i32, storage: Storage) {
    let partidx = 0;
    assert_eq!(
        code(outf.read_scanline_chunk_info(partidx, 42)),
        ErrorCode::NotOpenRead
    );

    assert_eq!(
        code(outf.get_name(partidx - 1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_name(partidx + count)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_name(partidx).unwrap(), b"beauty");

    assert_eq!(
        code(outf.get_storage(partidx - 1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_storage(partidx + count)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_storage(partidx).unwrap(), storage);
}

/// Port of `testStartWriteScan` (src/test/OpenEXRCoreTest/write.cpp:62-158) @ v3.5.3.
#[test]
fn test_start_write_scan() {
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testscan.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(outf.get_count(), 0);

    assert_eq!(
        code(outf.add_part(Some("beauty"), Storage::LAST_TYPE)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(outf.add_part(Some("beauty"), Storage::SCANLINE).unwrap(), 0);
    {
        outf.get_chunk_table_offset(0).expect("chunk table offset");
        assert_eq!(outf.get_count(), 1);
    }
    // dup name check
    assert_eq!(
        code(outf.add_part(Some("beauty"), Storage::SCANLINE)),
        ErrorCode::InvalidArgument
    );
    // this add should work, making 2 parts
    assert_eq!(outf.add_part(Some("other"), Storage::SCANLINE).unwrap(), 1);
    // test repeated add failure doesn't leak
    assert_eq!(
        code(outf.add_part(Some("beauty"), Storage::SCANLINE)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(outf.get_count(), 2);

    check_part0(&outf, 2, Storage::SCANLINE);

    assert_eq!(
        code(outf.set_tile_descriptor(
            0,
            32,
            32,
            TileLevelMode::ONE_LEVEL,
            TileRoundMode::ROUND_DOWN
        )),
        ErrorCode::TileScanMixedapi
    );

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testStartWriteDeepScan` (src/test/OpenEXRCoreTest/write.cpp:162-193) @ v3.5.3.
#[test]
fn test_start_write_deep_scan() {
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testdeepscan.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(
        outf.add_part(Some("beauty"), Storage::DEEP_SCANLINE)
            .unwrap(),
        0
    );
    assert_eq!(outf.get_count(), 1);

    assert_eq!(outf.get_storage(0).unwrap(), Storage::DEEP_SCANLINE);
    assert_eq!(outf.get_file_version_and_flags(), 2 | 0x00000800);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testStartWriteTile` (src/test/OpenEXRCoreTest/write.cpp:197-262) @ v3.5.3.
#[test]
fn test_start_write_tile() {
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testattr.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(outf.get_count(), 0);
    assert_eq!(outf.add_part(Some("beauty"), Storage::TILED).unwrap(), 0);
    assert_eq!(outf.get_count(), 1);

    check_part0(&outf, 1, Storage::TILED);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testStartWriteDeepTile` (src/test/OpenEXRCoreTest/write.cpp:266-297) @ v3.5.3.
#[test]
fn test_start_write_deep_tile() {
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testattr.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(
        outf.add_part(Some("beauty"), Storage::DEEP_TILED).unwrap(),
        0
    );
    assert_eq!(outf.get_count(), 1);

    assert_eq!(outf.get_storage(0).unwrap(), Storage::DEEP_TILED);
    assert_eq!(outf.get_file_version_and_flags(), 2 | 0x00000800);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// `exr_add_channel (outf, partidx, name, EXR_PIXEL_HALF, EXR_PERCEPTUALLY_LOGARITHMIC, 1, 1)`.
fn add_half(outf: &mut Context, partidx: i32, name: &str) -> Result<(), crate::context::Error> {
    outf.add_channel(
        partidx,
        Some(name),
        PixelType::HALF,
        PerceptualTreatment::LOGARITHMIC,
        1,
        1,
    )
}

/// Port of `testWriteBaseHeader` (src/test/OpenEXRCoreTest/write.cpp:299-697) @ v3.5.3.
#[test]
fn test_write_base_header() {
    let _defaults = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testattr.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(outf.get_count(), 0);
    assert_eq!(outf.add_part(Some("beauty"), Storage::TILED).unwrap(), 0);
    assert_eq!(outf.get_count(), 1);
    let partidx = 0;

    check_part0(&outf, 1, Storage::TILED);

    outf.set_longname_support(true).unwrap();
    outf.attr_set_int(partidx, Some("shortname"), 42).unwrap();
    outf.set_longname_support(false).unwrap();
    outf.set_longname_support(true).unwrap();
    add_half(
        &mut outf,
        partidx,
        "reallongreallongreallonglongchannelname",
    )
    .unwrap();
    assert_eq!(
        code(outf.set_longname_support(false)),
        ErrorCode::NameTooLong
    );
    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);

    let mut outf = start_write(&outfn);
    let partidx = outf.add_part(Some("beauty"), Storage::TILED).unwrap();

    assert_eq!(
        code(outf.attr_set_int(partidx, Some("areallongreallongreallonglongname"), 42)),
        ErrorCode::NameTooLong
    );

    outf.set_longname_support(true).unwrap();
    outf.attr_set_int(partidx, Some("areallongreallongreallonglongname"), 42)
        .unwrap();
    assert_eq!(
        code(outf.set_longname_support(false)),
        ErrorCode::NameTooLong
    );

    assert_eq!(
        code(outf.get_zip_compression_level(-1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_zip_compression_level(5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_zip_compression_level(0).unwrap(), -1);

    assert_eq!(
        code(outf.set_zip_compression_level(-1, 5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_zip_compression_level(5, 5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_zip_compression_level(0, -2)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(outf.set_zip_compression_level(0, 42)),
        ErrorCode::InvalidArgument
    );
    outf.set_zip_compression_level(0, 4).unwrap();
    assert_eq!(outf.get_zip_compression_level(0).unwrap(), 4);

    assert_eq!(
        code(outf.get_dwa_compression_level(-1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_dwa_compression_level(5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_dwa_compression_level(0).unwrap(), 45.0f32);

    assert_eq!(
        code(outf.set_dwa_compression_level(-1, 5.0)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_dwa_compression_level(5, 5.0)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_dwa_compression_level(0, -2.0)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(outf.set_dwa_compression_level(0, f32::INFINITY)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(outf.set_dwa_compression_level(0, f32::NAN)),
        ErrorCode::InvalidArgument
    );
    outf.set_dwa_compression_level(0, 42.0).unwrap();
    assert_eq!(outf.get_dwa_compression_level(0).unwrap(), 42.0f32);
    outf.set_dwa_compression_level(0, 420.0).unwrap();
    assert_eq!(outf.get_dwa_compression_level(0).unwrap(), 420.0f32);

    assert_eq!(
        code(outf.get_lossy_htj2k_quality(-1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_lossy_htj2k_quality(5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_lossy_htj2k_quality(0).unwrap(), 110.0f32);

    assert_eq!(
        code(outf.set_lossy_htj2k_quality(-1, 5.0)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_lossy_htj2k_quality(5, 5.0)),
        ErrorCode::ArgumentOutOfRange
    );
    for bad in [0.0f32, 151.0, f32::INFINITY, f32::NAN] {
        assert_eq!(
            code(outf.set_lossy_htj2k_quality(0, bad)),
            ErrorCode::InvalidArgument
        );
    }
    outf.set_lossy_htj2k_quality(0, 42.0).unwrap();
    assert_eq!(outf.get_lossy_htj2k_quality(0).unwrap(), 42.0f32);
    outf.set_lossy_htj2k_quality(0, 150.0).unwrap();
    assert_eq!(outf.get_lossy_htj2k_quality(0).unwrap(), 150.0f32);

    assert_eq!(
        code(outf.get_zstd_compression_level(-1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_zstd_compression_level(5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(outf.get_zstd_compression_level(0).unwrap(), 5);

    assert_eq!(
        code(outf.set_zstd_compression_level(-1, 5)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.set_zstd_compression_level(5, 5)),
        ErrorCode::ArgumentOutOfRange
    );
    for bad in [-1, 0, 23] {
        assert_eq!(
            code(outf.set_zstd_compression_level(0, bad)),
            ErrorCode::InvalidArgument
        );
    }
    for good in [10, 22, 1] {
        outf.set_zstd_compression_level(0, good).unwrap();
        assert_eq!(outf.get_zstd_compression_level(0).unwrap(), good);
    }

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);

    let mut outf = start_write(&outfn);
    let partidx = outf.add_part(Some("beauty"), Storage::SCANLINE).unwrap();
    let dataw = Box2i {
        min: V2i { x: -2, y: -3 },
        max: V2i { x: 514, y: 515 },
    };
    let dispw = Box2i {
        min: V2i { x: 0, y: 0 },
        max: V2i { x: 512, y: 512 },
    };
    let swc = V2f { x: 0.5, y: 0.5 };
    // (upstream passes the data window as the display window, and the other way round)
    outf.initialize_required_attr(
        partidx,
        Some(&dataw),
        Some(&dispw),
        1.0,
        Some(&swc),
        1.0,
        LineOrder::INCREASING_Y,
        Compression::ZIPS,
    )
    .unwrap();
    for c in ["R", "G", "B"] {
        add_half(&mut outf, partidx, c).unwrap();
    }

    outf.write_header().expect("exr_write_header");
    assert_eq!(outf.get_file_version_and_flags(), 2);
    assert_eq!(outf.get_chunk_table_offset(0).unwrap(), 364);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);

    ////

    let mut outf = start_write(&outfn);
    let partidx = outf.add_part(Some("beauty"), Storage::TILED).unwrap();
    let dataw = Box2i {
        min: V2i { x: 0, y: 0 },
        max: V2i { x: 512, y: 512 },
    };
    outf.initialize_required_attr(
        partidx,
        Some(&dataw),
        Some(&dispw),
        1.0,
        Some(&swc),
        1.0,
        LineOrder::INCREASING_Y,
        Compression::ZIPS,
    )
    .unwrap();
    for c in ["R", "G", "B"] {
        add_half(&mut outf, partidx, c).unwrap();
    }

    assert_eq!(code(outf.write_header()), ErrorCode::MissingReqAttr);
    outf.set_tile_descriptor(
        partidx,
        32,
        32,
        TileLevelMode::ONE_LEVEL,
        TileRoundMode::ROUND_DOWN,
    )
    .unwrap();

    assert_eq!(
        code(outf.get_tile_descriptor(-1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_tile_descriptor(partidx + 10)),
        ErrorCode::ArgumentOutOfRange
    );
    let (txsize, tysize, _, _) = outf.get_tile_descriptor(partidx).unwrap();
    assert!(txsize == 32 && tysize == 32);

    outf.write_header().expect("exr_write_header");
    assert_eq!(outf.get_file_version_and_flags(), 2 | 0x00000200);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);

    ////

    let mut outf = start_write(&outfn);
    assert_eq!(code(outf.write_header()), ErrorCode::FileBadHeader);
    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testUpdateMeta` (src/test/OpenEXRCoreTest/write.cpp:1459-1461) @ v3.5.3, which is
/// empty upstream.
#[test]
fn test_update_meta() {}

/// Port of `testWriteScans` (src/test/OpenEXRCoreTest/write.cpp:1463-1465) @ v3.5.3, which is
/// empty upstream.
#[test]
fn test_write_scans() {}

/// Port of `testWriteTiles` (src/test/OpenEXRCoreTest/write.cpp:1467-1554) @ v3.5.3.
#[test]
fn test_write_tiles() {
    let tempdir = make_temp_dir();
    let fn_ = format!("{}v1.7.test.tiled.exr", test_image_dir());
    let f = Context::start_read(Some(&fn_), Some(cinit())).expect("exr_start_read");
    let outfn = format!("{tempdir}v1.7.test.tiled.exr");
    let mut outf = start_write(&outfn);
    assert_eq!(outf.add_part(Some("test"), Storage::TILED).unwrap(), 0);

    outf.copy_unset_attributes(0, &f, 0).unwrap();

    assert_eq!(outf.get_storage(0).unwrap(), Storage::TILED);

    assert_eq!(outf.get_tile_levels(0).unwrap(), (1, 1));
    let (levelsx, levelsy) = outf.get_tile_sizes(0, 0, 0).unwrap();
    assert_eq!((levelsx, levelsy), (12, 24));

    // (exr_write_header on a read context: the Rust read context cannot be borrowed mutably
    // while it is shared, so the refusal is checked on a context of its own)
    let mut f2 = Context::start_read(Some(&fn_), Some(cinit())).expect("exr_start_read");
    assert_eq!(code(f2.write_header()), ErrorCode::NotOpenWrite);
    f2.finish().unwrap();
    outf.write_header().expect("exr_write_header");

    let partcnt = f.get_chunk_count(0).unwrap();
    let outpartcnt = outf.get_chunk_count(0).unwrap();
    assert_eq!(partcnt, outpartcnt);

    let dw = outf.get_data_window(0).unwrap();
    let mut cmem: Vec<u8> = Vec::new();
    let mut ty = 0;
    let mut y = dw.min.y;
    while y <= dw.max.y {
        let mut tx = 0;
        let mut x = dw.min.x;
        while x <= dw.max.x {
            let cinfo = f.read_tile_chunk_info(0, tx, ty, 0, 0).unwrap();
            if cmem.len() < cinfo.packed_size as usize {
                cmem = vec![0u8; cinfo.packed_size as usize];
            }
            let chunk = &mut cmem[..cinfo.packed_size as usize];
            f.read_chunk(0, &cinfo, Some(&mut *chunk)).unwrap();
            outf.write_tile_chunk(0, tx, ty, 0, 0, Some(chunk)).unwrap();
            tx += 1;
            x += levelsx;
        }
        ty += 1;
        y += levelsy;
    }
    f.finish().unwrap();
    outf.finish().unwrap();

    let testf = Context::start_read(Some(&outfn), Some(cinit())).expect("exr_start_read");
    assert_eq!(testf.get_tile_levels(0).unwrap(), (1, 1));
    assert_eq!(testf.get_tile_sizes(0, 0, 0).unwrap(), (12, 24));
    testf.finish().unwrap();

    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testWriteMultiPart` (src/test/OpenEXRCoreTest/write.cpp:1556-1611) @ v3.5.3.
#[test]
fn test_write_multi_part() {
    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testdeepscan.exr");
    let mut outf = start_write(&outfn);

    assert_eq!(outf.add_part(Some("beauty"), Storage::SCANLINE).unwrap(), 0);
    assert_eq!(outf.add_part(Some("debug"), Storage::TILED).unwrap(), 1);
    assert_eq!(outf.get_count(), 2);

    // the part table grows geometrically, so exercise several reallocations
    for p in 2..100 {
        let name = format!("part{p}");
        assert_eq!(outf.add_part(Some(&name), Storage::SCANLINE).unwrap(), p);
    }
    assert_eq!(outf.get_count(), 100);
    for p in 2..100 {
        let name = format!("part{p}");
        assert_eq!(outf.get_name(p).unwrap(), name.as_bytes());
    }

    assert_eq!(outf.get_storage(0).unwrap(), Storage::SCANLINE);
    assert_eq!(outf.get_storage(1).unwrap(), Storage::TILED);

    assert_eq!(outf.get_file_version_and_flags(), 2 | 0x00001000);

    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);
}

/// Port of `testStartWriteUTF8` (src/test/OpenEXRCoreTest/write.cpp:1613-1688) @ v3.5.3.
#[test]
fn test_start_write_utf8() {
    let _defaults = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let tempdir = make_temp_dir();
    // per google translate, image in Japanese
    let outfn = format!("{tempdir}\u{753b}\u{50cf}.exr");

    let init = Initializer {
        zip_level: 3,
        flags: crate::context::EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER,
        ..cinit()
    };

    crate::base::set_default_zip_compression_level(-1);

    let mut outf = Context::start_write(
        Some(&outfn),
        DefaultWriteMode::WriteFileDirectly,
        Some(init),
    )
    .expect("exr_start_write");
    assert_eq!(outf.add_part(Some("beauty"), Storage::SCANLINE).unwrap(), 0);
    assert_eq!(outf.get_count(), 1);
    let partidx = 0;

    let (fw, fh) = (1, 1);
    let data_w = Box2i {
        min: V2i { x: 0, y: 0 },
        max: V2i { x: 0, y: 0 },
    };

    outf.initialize_required_attr_simple(partidx, fw, fh, Compression::NONE)
        .unwrap();
    outf.set_data_window(partidx, Some(&data_w)).unwrap();

    add_half(&mut outf, partidx, "h").unwrap();
    outf.write_header().expect("exr_write_header");

    let cinfo = outf.write_scanline_chunk_info(0, 0).unwrap();
    let mut encoder = outf.encoding_initialize(0, &cinfo).unwrap();

    let hval: [u16; 2] = [0x1234, 0];
    let hbytes: Vec<u8> = hval.iter().flat_map(|v| v.to_ne_bytes()).collect();
    for c in encoder.channels.iter_mut() {
        c.data = Some(BufferRef {
            buffer: 0,
            offset: 0,
        });
        c.user_pixel_stride = 2;
        c.user_line_stride = 2;
    }
    outf.encoding_choose_default_routines(0, &mut encoder)
        .unwrap();
    outf.encoding_run(0, &mut encoder, &[&hbytes]).unwrap();
    outf.encoding_destroy(&mut encoder).unwrap();

    outf.finish().expect("exr_finish");
    assert!(std::path::Path::new(&outfn).exists());
    let _ = std::fs::remove_file(&outfn);
}

/// The two calls for parts -1 and 1 of a one-part context, both out of range.
fn oor<T: std::fmt::Debug>(mut f: impl FnMut(i32) -> Result<T, crate::context::Error>) {
    assert_eq!(code(f(-1)), ErrorCode::ArgumentOutOfRange);
    assert_eq!(code(f(1)), ErrorCode::ArgumentOutOfRange);
}

/// The getter half of upstream's `TEST_CORNER_CASE_NAME*` macros: parts -1 and 1, a NULL
/// name and an empty one refused, then the attribute's value.
macro_rules! get_cases {
    ($outf:ident, $get:ident, $name:literal) => {{
        oor(|p| $outf.$get(p, Some($name)));
        assert_eq!(code($outf.$get(0, None)), ErrorCode::InvalidArgument);
        assert_eq!(code($outf.$get(0, Some(""))), ErrorCode::InvalidArgument);
        $outf.$get(0, Some($name)).unwrap()
    }};
}

/// `TEST_CORNER_CASE_NAME (fnt, arg, alt)` (src/test/OpenEXRCoreTest/write.cpp:838-874
/// @ v3.5.3), for a setter taking the value by pointer and `alt` int.
macro_rules! name_case {
    ($outf:ident, $set:ident, $get:ident, $name:literal, $val:expr) => {{
        let v = $val;
        oor(|p| $outf.$set(p, Some($name), Some(&v)));
        assert_eq!(
            code($outf.$set(0, Some($name), None)),
            ErrorCode::InvalidArgument
        );
        $outf.$set(0, Some($name), Some(&v)).unwrap();
        assert_eq!(
            code($outf.attr_set_int(0, Some($name), 0)),
            ErrorCode::AttrTypeMismatch
        );
        get_cases!($outf, $get, $name)
    }};
}

/// `TEST_CORNER_CASE_NAME_V (fnt, arg, alt)` (src/test/OpenEXRCoreTest/write.cpp:913-946
/// @ v3.5.3), for a setter taking the value itself.
macro_rules! name_case_v {
    ($outf:ident, $set:ident, $get:ident, $alt:ident, $altv:expr, $name:literal, $val:expr) => {{
        let v = $val;
        oor(|p| $outf.$set(p, Some($name), v));
        $outf.$set(0, Some($name), v).unwrap();
        assert_eq!(
            code($outf.$alt(0, Some($name), $altv)),
            ErrorCode::AttrTypeMismatch
        );
        get_cases!($outf, $get, $name)
    }};
}

/// Port of `testWriteAttrs` (src/test/OpenEXRCoreTest/write.cpp:757-1458) @ v3.5.3. Left out:
/// the access mode 42, which `AttrListAccessMode` cannot hold.
#[test]
fn test_write_attrs() {
    use crate::bytes::AttrBytes;
    use crate::openexr_attr::{
        Box2f, Chromaticities, Envmap, Keycode, M33d, M33f, M44d, M44f, Rational, TileDesc,
        Timecode, V2d, V3d, V3f, V3i,
    };
    use crate::part_attr::{AttrListAccessMode, UserAttr};
    use crate::preview::AttrPreview;

    let tempdir = make_temp_dir();
    let outfn = format!("{tempdir}testattr.exr");
    let mut outf = start_write(&outfn);
    let partidx = outf.add_part(Some("tester"), Storage::SCANLINE).unwrap();
    outf.initialize_required_attr_simple(partidx, 1, 1, Compression::ZIPS)
        .unwrap();
    for c in ["R", "G", "B"] {
        add_half(&mut outf, partidx, c).unwrap();
    }

    oor(|p| outf.set_name(p, Some("a")));
    assert_eq!(
        code(outf.set_name(partidx, None)),
        ErrorCode::InvalidArgument
    );
    outf.set_name(partidx, Some("")).unwrap();
    outf.set_name(partidx, Some("beauty")).unwrap();
    assert_eq!(outf.get_name(partidx).unwrap(), b"beauty");

    oor(|p| outf.get_channels(p));
    let chans = outf.get_channels(partidx).unwrap().clone();

    oor(|p| outf.set_channels(p, &chans));
    outf.set_channels(partidx, &chans).unwrap();
    let chans = outf.get_channels(partidx).unwrap().clone();

    {
        assert_eq!(
            code(outf.set_version(partidx, 2)),
            ErrorCode::ArgumentOutOfRange
        );
        assert_eq!(
            code(outf.set_version(partidx, 0)),
            ErrorCode::ArgumentOutOfRange
        );
        oor(|p| outf.set_version(p, 1));
        outf.set_version(partidx, 1).unwrap();
        oor(|p| outf.get_version(p));
        assert_eq!(outf.get_version(partidx).unwrap(), 1);
    }

    {
        oor(|p| outf.get_compression(p));
        assert_eq!(outf.get_compression(partidx).unwrap(), Compression::ZIPS);
        oor(|p| outf.set_compression(p, Compression::ZIP));
        outf.set_compression(partidx, Compression::ZIP).unwrap();
        assert_eq!(outf.get_compression(partidx).unwrap(), Compression::ZIP);
    }

    {
        oor(|p| outf.set_chunk_count(p, 1));
        outf.set_chunk_count(partidx, 1).unwrap();
    }

    {
        oor(|p| outf.get_data_window(p));
        let dw = outf.get_data_window(partidx).unwrap();
        oor(|p| outf.set_data_window(p, Some(&dw)));
        outf.set_data_window(partidx, Some(&dw)).unwrap();
    }

    {
        oor(|p| outf.get_display_window(p));
        let dpw = outf.get_display_window(partidx).unwrap();
        oor(|p| outf.set_display_window(p, Some(&dpw)));
        outf.set_display_window(partidx, Some(&dpw)).unwrap();
    }

    {
        let mybytes = AttrBytes {
            size: 4,
            data: Some(vec![0x76, 0x2f, 0x31, 0x01]),
            hint_length: 0,
            type_hint: None,
        };
        name_case!(outf, attr_set_bytes, attr_get_bytes, "mybytes", mybytes);
    }

    {
        oor(|p| outf.get_lineorder(p));
        assert_eq!(
            outf.get_lineorder(partidx).unwrap(),
            LineOrder::INCREASING_Y
        );
        oor(|p| outf.set_lineorder(p, LineOrder::DECREASING_Y));
        outf.set_lineorder(partidx, LineOrder::DECREASING_Y)
            .unwrap();
        assert_eq!(
            outf.get_lineorder(partidx).unwrap(),
            LineOrder::DECREASING_Y
        );
        outf.set_lineorder(partidx, LineOrder::INCREASING_Y)
            .unwrap();
    }

    {
        oor(|p| outf.get_pixel_aspect_ratio(p));
        outf.get_pixel_aspect_ratio(partidx).unwrap();
        oor(|p| outf.set_pixel_aspect_ratio(p, 1.0));
        outf.set_pixel_aspect_ratio(partidx, 1.0).unwrap();
    }

    {
        oor(|p| outf.get_screen_window_center(p));
        let swc = outf.get_screen_window_center(partidx).unwrap();
        oor(|p| outf.set_screen_window_center(p, Some(&swc)));
        outf.set_screen_window_center(partidx, Some(&swc)).unwrap();
    }

    {
        oor(|p| outf.get_screen_window_width(p));
        outf.get_screen_window_width(partidx).unwrap();
        oor(|p| outf.set_screen_window_width(p, 1.0));
        outf.set_screen_window_width(partidx, 1.0).unwrap();
    }

    {
        let intv = name_case_v!(
            outf,
            attr_set_int,
            attr_get_int,
            attr_set_float,
            0.0,
            "intv",
            42
        );
        assert_eq!(intv, 42);
    }

    {
        let floatv = name_case_v!(
            outf,
            attr_set_float,
            attr_get_float,
            attr_set_int,
            0,
            "floatv",
            42.0f32
        );
        assert_eq!(floatv, 42.0f32);
    }

    {
        let doublev = name_case_v!(
            outf,
            attr_set_double,
            attr_get_double,
            attr_set_int,
            0,
            "doublev",
            42.0f64
        );
        assert_eq!(doublev, 42.0f64);
    }

    {
        // TEST_CORNER_CASE_NAME_NOALT (channels, addlchans, addlchans)
        let addlchans = chans.clone();
        oor(|p| outf.attr_set_channels(p, Some("addlchans"), Some(&addlchans)));
        assert_eq!(
            code(outf.attr_set_channels(partidx, Some("addlchans"), None)),
            ErrorCode::InvalidArgument
        );
        outf.attr_set_channels(partidx, Some("addlchans"), Some(&addlchans))
            .unwrap();
        get_cases!(outf, attr_get_channels, "addlchans");
    }

    {
        let mychroma = Chromaticities {
            red_x: 1.0,
            red_y: 2.0,
            green_x: 3.0,
            green_y: 4.0,
            blue_x: 5.0,
            blue_y: 6.0,
            white_x: 7.0,
            white_y: 8.0,
        };
        let mychroma = name_case!(
            outf,
            attr_set_chromaticities,
            attr_get_chromaticities,
            "mychroma",
            mychroma
        );
        assert_eq!(mychroma.red_y, 2.0);
        assert_eq!(mychroma.white_x, 7.0);
    }

    {
        let compt = name_case_v!(
            outf,
            attr_set_compression,
            attr_get_compression,
            attr_set_int,
            0,
            "compt",
            Compression::ZIPS
        );
        assert_eq!(compt, Compression::ZIPS);
    }

    {
        let envmapt = name_case_v!(
            outf,
            attr_set_envmap,
            attr_get_envmap,
            attr_set_int,
            0,
            "envmapt",
            Envmap::LATLONG
        );
        assert_eq!(envmapt, Envmap::LATLONG);
    }

    {
        // TEST_CORNER_CASE_NAME_C (float_vector, myfvec, fvsz, &outfvec, int)
        let myfvec = [1.0f32, 2.0];
        let fvsz = 2;
        oor(|p| outf.attr_set_float_vector(p, Some("myfvec"), fvsz, Some(&myfvec)));
        assert_eq!(
            code(outf.attr_set_float_vector(partidx, Some("myfvec"), -1, Some(&myfvec))),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(outf.attr_set_float_vector(partidx, Some("myfvec"), fvsz, None)),
            ErrorCode::InvalidArgument
        );
        outf.attr_set_float_vector(partidx, Some("myfvec"), fvsz, Some(&myfvec))
            .unwrap();
        assert_eq!(
            code(outf.attr_set_int(partidx, Some("myfvec"), 0)),
            ErrorCode::AttrTypeMismatch
        );
        let (fvsz, outfvec) = get_cases!(outf, attr_get_float_vector, "myfvec");
        assert_eq!(fvsz, 2);
        assert_eq!(outfvec[1], 2.0f32);
    }

    {
        let mykeycodet = Keycode {
            film_mfc_code: 1,
            film_type: 2,
            prefix: 3,
            count: 4,
            perf_offset: 5,
            perfs_per_frame: 6,
            perfs_per_count: 7,
        };
        let mykeycodet = name_case!(
            outf,
            attr_set_keycode,
            attr_get_keycode,
            "mykeycodet",
            mykeycodet
        );
        assert_eq!(mykeycodet.count, 4);
    }

    {
        let mylineordt = name_case_v!(
            outf,
            attr_set_lineorder,
            attr_get_lineorder,
            attr_set_int,
            0,
            "mylineordt",
            LineOrder::DECREASING_Y
        );
        assert_eq!(mylineordt, LineOrder::DECREASING_Y);
    }

    {
        let tb2i = Box2i {
            min: V2i { x: 1, y: 2 },
            max: V2i { x: 3, y: 4 },
        };
        let tb2i = name_case!(outf, attr_set_box2i, attr_get_box2i, "tb2i", tb2i);
        assert_eq!(tb2i.min.x, 1);
        assert_eq!(tb2i.min.y, 2);
        assert_eq!(tb2i.max.x, 3);
        assert_eq!(tb2i.max.y, 4);
    }

    {
        let tb2f = Box2f {
            min: V2f { x: 1.0, y: 2.0 },
            max: V2f { x: 3.0, y: 4.0 },
        };
        let tb2f = name_case!(outf, attr_set_box2f, attr_get_box2f, "tb2f", tb2f);
        assert_eq!(tb2f.min.x, 1.0);
        assert_eq!(tb2f.min.y, 2.0);
        assert_eq!(tb2f.max.x, 3.0);
        assert_eq!(tb2f.max.y, 4.0);
    }

    {
        let tv2i = name_case!(outf, attr_set_v2i, attr_get_v2i, "tv2i", V2i { x: 1, y: 2 });
        assert_eq!(tv2i.x, 1);
        assert_eq!(tv2i.y, 2);
    }

    {
        let m = M33f {
            m: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        };
        let mym33f = name_case!(outf, attr_set_m33f, attr_get_m33f, "mym33f", m);
        assert_eq!(mym33f.m[3], 4.0);
    }

    {
        let m = M33d {
            m: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0],
        };
        let mym33d = name_case!(outf, attr_set_m33d, attr_get_m33d, "mym33d", m);
        assert_eq!(mym33d.m[3], 4.0);
    }

    {
        let mut m = M44f { m: [0.0; 16] };
        for (i, v) in m.m.iter_mut().enumerate() {
            *v = (i + 1) as f32;
        }
        let mym44f = name_case!(outf, attr_set_m44f, attr_get_m44f, "mym44f", m);
        assert_eq!(mym44f.m[3], 4.0);
    }

    {
        let mut m = M44d { m: [0.0; 16] };
        for (i, v) in m.m.iter_mut().enumerate() {
            *v = (i + 1) as f64;
        }
        let mym44d = name_case!(outf, attr_set_m44d, attr_get_m44d, "mym44d", m);
        assert_eq!(mym44d.m[3], 4.0);
    }

    {
        let dummy: Vec<u8> = [0xDE, 0xAD, 0xBE, 0xEF].repeat(4);
        let mypreview = AttrPreview::borrowed(2, 2, Some(&dummy));
        name_case!(
            outf,
            attr_set_preview,
            attr_get_preview,
            "mypreview",
            mypreview
        );
    }

    {
        let myrational = Rational {
            num: 24000,
            denom: 1001,
        };
        let myrational = name_case!(
            outf,
            attr_set_rational,
            attr_get_rational,
            "myrational",
            myrational
        );
        assert_eq!(myrational.num, 24000);
        assert_eq!(myrational.denom, 1001);
    }

    {
        let sname = "teststrattr";
        let scal = "testvalue";
        oor(|p| outf.attr_set_string(p, Some(sname), Some(scal)));
        outf.attr_set_string(partidx, Some(sname), None).unwrap();
        outf.attr_set_string(partidx, Some(sname), Some(""))
            .unwrap();
        outf.attr_set_string(partidx, Some(sname), None).unwrap();
        outf.attr_set_string(partidx, Some(sname), Some(scal))
            .unwrap();
        assert_eq!(
            code(outf.attr_set_int(partidx, Some(sname), 0)),
            ErrorCode::AttrTypeMismatch
        );
        oor(|p| outf.attr_get_string(p, Some(sname)));
        assert_eq!(
            code(outf.attr_get_string(partidx, None)),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(outf.attr_get_string(partidx, Some(""))),
            ErrorCode::InvalidArgument
        );
        let (outssz, outsv) = outf.attr_get_string(partidx, Some(sname)).unwrap();
        assert_eq!(outssz, 9);
        assert_eq!(outsv, scal.as_bytes());
    }

    {
        // TEST_CORNER_CASE_NAME_C (string_vector, mysvec, svsz, outsvec, int)
        let mysvec = [Some("foo"), Some("bar")];
        let svsz = 2;
        oor(|p| outf.attr_set_string_vector(p, Some("mysvec"), svsz, Some(&mysvec)));
        assert_eq!(
            code(outf.attr_set_string_vector(partidx, Some("mysvec"), -1, Some(&mysvec))),
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            code(outf.attr_set_string_vector(partidx, Some("mysvec"), svsz, None)),
            ErrorCode::InvalidArgument
        );
        outf.attr_set_string_vector(partidx, Some("mysvec"), svsz, Some(&mysvec))
            .unwrap();
        assert_eq!(
            code(outf.attr_set_int(partidx, Some("mysvec"), 0)),
            ErrorCode::AttrTypeMismatch
        );
        let outsvec = get_cases!(outf, attr_get_string_vector, "mysvec");
        assert_eq!(outsvec.len(), 2);
    }

    {
        // EXR_PACK_TILE_LEVEL_ROUND (EXR_TILE_MIPMAP_LEVELS, EXR_TILE_ROUND_DOWN)
        let mytiled = TileDesc {
            x_size: 32,
            y_size: 32,
            level_and_round: (((TileRoundMode::ROUND_DOWN.0 & 0xF) << 4)
                | (TileLevelMode::MIPMAP_LEVELS.0 & 0xF)) as u8,
        };
        name_case!(
            outf,
            attr_set_tiledesc,
            attr_get_tiledesc,
            "mytiled",
            mytiled
        );
    }

    {
        let mytimecode = Timecode {
            time_and_flags: 42,
            user_data: 84,
        };
        let mytimecode = name_case!(
            outf,
            attr_set_timecode,
            attr_get_timecode,
            "mytimecode",
            mytimecode
        );
        assert_eq!(mytimecode.time_and_flags, 42);
        assert_eq!(mytimecode.user_data, 84);
    }

    {
        let tv2f = name_case!(
            outf,
            attr_set_v2f,
            attr_get_v2f,
            "tv2f",
            V2f { x: 1.0, y: 2.0 }
        );
        assert_eq!((tv2f.x, tv2f.y), (1.0, 2.0));
    }

    {
        let tv2d = name_case!(
            outf,
            attr_set_v2d,
            attr_get_v2d,
            "tv2d",
            V2d { x: 1.0, y: 2.0 }
        );
        assert_eq!((tv2d.x, tv2d.y), (1.0, 2.0));
    }

    {
        let tv3i = name_case!(
            outf,
            attr_set_v3i,
            attr_get_v3i,
            "tv3i",
            V3i { x: 1, y: 2, z: 3 }
        );
        assert_eq!((tv3i.x, tv3i.y, tv3i.z), (1, 2, 3));
    }

    {
        let tv3f = name_case!(
            outf,
            attr_set_v3f,
            attr_get_v3f,
            "tv3f",
            V3f {
                x: 1.0,
                y: 2.0,
                z: 3.0
            }
        );
        assert_eq!((tv3f.x, tv3f.y, tv3f.z), (1.0, 2.0, 3.0));
    }

    {
        let tv3d = name_case!(
            outf,
            attr_set_v3d,
            attr_get_v3d,
            "tv3d",
            V3d {
                x: 1.0,
                y: 2.0,
                z: 3.0
            }
        );
        assert_eq!((tv3d.x, tv3d.y, tv3d.z), (1.0, 2.0, 3.0));
    }

    {
        outf.attr_set_user(
            partidx,
            Some("tuser"),
            Some("mytype"),
            4,
            None,
            Some(b"foo\0"),
        )
        .unwrap();
        match outf.attr_get_user(partidx, Some("tuser")).unwrap() {
            UserAttr::Packed {
                type_name,
                size,
                data,
            } => {
                assert_eq!(type_name, b"mytype");
                assert_eq!(size, 4);
                let data = data.expect("data");
                let end = data.iter().position(|&b| b == 0).unwrap();
                assert_eq!(&data[..end], b"foo");
            }
            UserAttr::Unpacked { .. } => panic!("no handler is registered for mytype"),
        }
    }

    oor(|p| outf.get_attribute_count(p));
    let attrcnt = outf.get_attribute_count(partidx).unwrap();
    println!("Testing writing of {attrcnt} attributes");

    oor(|p| outf.get_attribute_by_index(p, AttrListAccessMode::FileOrder, 15));
    assert_eq!(
        code(outf.get_attribute_by_index(partidx, AttrListAccessMode::FileOrder, -1)),
        ErrorCode::ArgumentOutOfRange
    );
    assert_eq!(
        code(outf.get_attribute_by_index(partidx, AttrListAccessMode::FileOrder, 1234)),
        ErrorCode::ArgumentOutOfRange
    );
    outf.get_attribute_by_index(partidx, AttrListAccessMode::FileOrder, 15)
        .unwrap();
    outf.get_attribute_by_index(partidx, AttrListAccessMode::SortedOrder, 15)
        .unwrap();

    oor(|p| outf.get_attribute_by_name(p, Some("tv3f")));
    assert_eq!(
        code(outf.get_attribute_by_name(partidx, None)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(outf.get_attribute_by_name(partidx, Some(""))),
        ErrorCode::InvalidArgument
    );
    outf.get_attribute_by_name(partidx, Some("tv3f")).unwrap();

    oor(|p| outf.get_attribute_list(p, AttrListAccessMode::FileOrder));
    let n = outf
        .get_attribute_list(partidx, AttrListAccessMode::FileOrder)
        .unwrap()
        .len();
    assert_eq!(n as i32, attrcnt);
    outf.get_attribute_list(partidx, AttrListAccessMode::SortedOrder)
        .unwrap();

    outf.write_header().expect("exr_write_header");
    let cinfo = outf.write_scanline_chunk_info(0, 0).unwrap();
    let mut encoder = outf.encoding_initialize(0, &cinfo).unwrap();
    let rgb = [0u8; 6];
    for (c, off) in [(0usize, 4usize), (1, 2), (2, 0)] {
        encoder.channels[c].data = Some(BufferRef {
            buffer: 0,
            offset: off,
        });
        encoder.channels[c].user_pixel_stride = 6;
        encoder.channels[c].user_line_stride = 6;
    }
    outf.encoding_choose_default_routines(0, &mut encoder)
        .unwrap();
    outf.encoding_run(0, &mut encoder, &[&rgb]).unwrap();
    outf.encoding_destroy(&mut encoder).unwrap();
    outf.finish().expect("exr_finish");

    let outf = Context::start_read(Some(&outfn), Some(cinit())).expect("exr_start_read");
    outf.finish().expect("exr_finish");
    let _ = std::fs::remove_file(&outfn);

    test_copy_attrs_long_channel_name(&tempdir);
}

/// Port of `testCopyAttrsLongChannelName` (src/test/OpenEXRCoreTest/write.cpp:699-755)
/// @ v3.5.3, which testWriteAttrs runs: copying a header whose channel name is longer than the
/// destination context allows must fail without leaving a dangling cached channel list.
fn test_copy_attrs_long_channel_name(tempdir: &str) {
    let srcfn = format!("{tempdir}testcopylong.exr");
    let longname = "ChannelNameThatIsLongerThanThirtyOneBytes_0123456789";

    let mut outf =
        Context::start_write(Some(&srcfn), DefaultWriteMode::WriteFileDirectly, None).unwrap();
    outf.set_longname_support(true).unwrap();
    let partidx = outf.add_part(Some("src"), Storage::SCANLINE).unwrap();
    outf.initialize_required_attr_simple(partidx, 1, 1, Compression::NONE)
        .unwrap();
    add_half(&mut outf, partidx, longname).unwrap();
    outf.write_header().unwrap();

    let hval = 0u16.to_ne_bytes();
    let cinfo = outf.write_scanline_chunk_info(0, 0).unwrap();
    let mut encoder = outf.encoding_initialize(0, &cinfo).unwrap();
    encoder.channels[0].data = Some(BufferRef {
        buffer: 0,
        offset: 0,
    });
    encoder.channels[0].user_pixel_stride = 2;
    encoder.channels[0].user_line_stride = 2;
    outf.encoding_choose_default_routines(0, &mut encoder)
        .unwrap();
    outf.encoding_run(0, &mut encoder, &[&hval]).unwrap();
    outf.encoding_destroy(&mut encoder).unwrap();
    outf.finish().unwrap();

    let srcf = Context::start_read(Some(&srcfn), None).unwrap();
    let dstfn = format!("{tempdir}testcopylong_out.exr");
    let mut dstf =
        Context::start_write(Some(&dstfn), DefaultWriteMode::WriteFileDirectly, None).unwrap();
    let partidx = dstf.add_part(Some("dst"), Storage::SCANLINE).unwrap();

    assert!(dstf.copy_unset_attributes(partidx, &srcf, 0).is_err());

    // none of these may touch freed memory; the channel list is simply missing, so it can be
    // repaired by adding a channel
    assert!(dstf.write_header().is_err());
    add_half(&mut dstf, partidx, "R").unwrap();

    dstf.finish().unwrap();
    srcf.finish().unwrap();
    let _ = std::fs::remove_file(&srcfn);
    let _ = std::fs::remove_file(&dstfn);
}
