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
