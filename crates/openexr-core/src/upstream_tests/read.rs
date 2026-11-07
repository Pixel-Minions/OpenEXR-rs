// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/read.cpp` @ v3.5.3.

use std::sync::Arc;

use crate::coding::BufferRef;
use crate::context::{Context, ErrorReport, Initializer};
use crate::errors::ErrorCode;
use crate::openexr_attr::{Compression, PixelType, Storage};

/// `ILM_IMF_TEST_IMAGEDIR` (src/test/OpenEXRCoreTest/CMakeLists.txt:27 @ v3.5.3): upstream's
/// test images, with a trailing separator.
pub(crate) fn test_image_dir() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../upstream/openexr/src/test/OpenEXRTest/"
    )
    .to_string()
}

/// `makeTempDir` (src/test/OpenEXRCoreTest/main.cpp:65-120 @ v3.5.3): a fresh directory under
/// the system temporary directory, with a trailing separator.
pub(crate) fn make_temp_dir() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("OpenEXRTest_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temporary directory");
    let mut s = dir.to_string_lossy().into_owned();
    s.push(std::path::MAIN_SEPARATOR);
    s
}

/// `err_cb` (src/test/OpenEXRCoreTest/read.cpp:19-23 @ v3.5.3).
fn err_cb(r: &ErrorReport<'_>) {
    eprintln!(
        "err_cb ERROR {}: {}",
        r.code,
        String::from_utf8_lossy(r.message)
    );
}

/// `cinit` with `cinit.error_handler_fn = &err_cb`.
fn cinit() -> Option<Initializer> {
    Some(Initializer {
        error_handler: Some(Arc::new(err_cb)),
        ..Initializer::default()
    })
}

/// Port of `testReadBadArgs` (src/test/OpenEXRCoreTest/read.cpp:65-106) @ v3.5.3. Left out:
/// the NULL context pointer and the malloc failure (PLAN.md O8).
#[test]
fn test_read_bad_args() {
    let tempdir = make_temp_dir();
    let code = |r: Result<Context, crate::context::Error>| r.unwrap_err().code;
    assert_eq!(
        code(Context::start_read(None, None)),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(Context::start_read(None, cinit())),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        code(Context::start_read(Some(""), cinit())),
        ErrorCode::FileAccess
    );
    // windows fails on directory open, where under unix you can open the directory as a file
    // handle but not read from it
    let dir_code = if cfg!(windows) {
        ErrorCode::FileAccess
    } else {
        ErrorCode::ReadIo
    };
    assert_eq!(code(Context::start_read(Some(&tempdir), cinit())), dir_code);
    let fn_ = format!("{tempdir}invalid.exr");
    assert_eq!(
        code(Context::start_read(Some(&fn_), cinit())),
        ErrorCode::FileAccess
    );
}

/// Port of `testReadBadFiles` (src/test/OpenEXRCoreTest/read.cpp:107-123) @ v3.5.3.
#[test]
fn test_read_bad_files() {
    let fn_ = format!("{}invalid.exr", test_image_dir());
    assert_eq!(
        Context::start_read(Some(&fn_), cinit()).unwrap_err().code,
        ErrorCode::FileBadHeader
    );
    assert_eq!(
        Context::test_file_header(Some("somenonexistentexrfile.exr"), cinit())
            .unwrap_err()
            .code,
        ErrorCode::FileAccess
    );
}

/// Port of `testOpenScans` (src/test/OpenEXRCoreTest/read.cpp:211-234) @ v3.5.3.
#[test]
fn test_open_scans() {
    for name in [
        "v1.7.test.1.exr",
        "v1.7.test.planar.exr",
        "v1.7.test.interleaved.exr",
    ] {
        let fn_ = format!("{}{name}", test_image_dir());
        let f = Context::start_read(Some(&fn_), cinit()).expect(name);
        f.finish().expect("finish");
    }
}

/// Port of `testOpenTiles` (src/test/OpenEXRCoreTest/read.cpp:235-252) @ v3.5.3.
#[test]
fn test_open_tiles() {
    for name in ["tiled.exr", "v1.7.test.tiled.exr"] {
        let fn_ = format!("{}{name}", test_image_dir());
        let f = Context::start_read(Some(&fn_), cinit()).expect(name);
        f.finish().expect("finish");
    }
}

/// Port of `testOpenMultiPart` (src/test/OpenEXRCoreTest/read.cpp:253-255) @ v3.5.3: upstream's
/// body is empty.
#[test]
fn test_open_multi_part() {}

/// Port of `testReadMeta` (src/test/OpenEXRCoreTest/read.cpp:124-209) @ v3.5.3. Left out: the
/// NULL context and output pointers (PLAN.md O8); `exr_get_user_data`'s `NULL` is `None`.
#[test]
fn test_read_meta() {
    let fn_ = format!("{}v1.7.test.1.exr", test_image_dir());
    Context::test_file_header(Some(&fn_), cinit()).expect("test_file_header");
    let mut f = Context::start_read(Some(&fn_), cinit()).expect("start_read");
    assert_eq!(f.get_file_version_and_flags(), 2);

    let oor = ErrorCode::ArgumentOutOfRange;
    assert_eq!(f.get_chunk_table_offset(-1).unwrap_err().code, oor);
    assert_eq!(f.get_chunk_table_offset(2).unwrap_err().code, oor);
    assert_eq!(f.get_chunk_table_offset(0).unwrap(), 331);

    let now = ErrorCode::NotOpenWrite;
    assert_eq!(
        f.attr_declare_by_type(0, Some("foo"), Some("box2i"))
            .unwrap_err()
            .code,
        now
    );
    assert_eq!(
        f.attr_declare(0, Some("bar"), crate::openexr_attr::AttributeType::BOX2I)
            .unwrap_err()
            .code,
        now
    );
    assert_eq!(
        f.add_part(Some("beauty"), crate::openexr_attr::Storage::TILED)
            .unwrap_err()
            .code,
        now
    );
    assert_eq!(f.set_longname_support(false).unwrap_err().code, now);
    assert_eq!(f.set_longname_support(true).unwrap_err().code, now);

    assert!(f.get_user_data().is_none());

    assert_eq!(f.get_zip_compression_level(0).unwrap(), -1);
    assert_eq!(f.set_zip_compression_level(0, 4).unwrap_err().code, now);
    assert_eq!(f.get_dwa_compression_level(0).unwrap(), 45.0);
    assert_eq!(f.set_dwa_compression_level(0, 42.0).unwrap_err().code, now);
    assert_eq!(f.get_zstd_compression_level(0).unwrap(), 5);
    assert_eq!(f.set_zstd_compression_level(0, 5).unwrap_err().code, now);
    f.finish().expect("finish");
}

/// Port of `testReadScans` (src/test/OpenEXRCoreTest/read.cpp:257-403) @ v3.5.3. Left out: the
/// NULL context and output calls (PLAN.md O8). The channels name the output buffers by index
/// (`BufferRef`) where upstream sets `decode_to_ptr`; `exr_finish` is the drop.
#[test]
fn test_read_scans() {
    let fn_ = test_image_dir() + "v1.7.test.interleaved.exr";
    let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

    let oor = ErrorCode::ArgumentOutOfRange;
    assert_eq!(f.get_chunk_count(-1).unwrap_err().code, oor);
    assert_eq!(f.get_chunk_count(11).unwrap_err().code, oor);
    f.get_chunk_count(0).unwrap();

    assert_eq!(f.get_scanlines_per_chunk(-1).unwrap_err().code, oor);
    assert_eq!(f.get_scanlines_per_chunk(11).unwrap_err().code, oor);
    let ccount = f.get_scanlines_per_chunk(0).unwrap();
    assert_eq!(ccount, 1);

    assert_eq!(f.get_data_window(-1).unwrap_err().code, oor);
    assert_eq!(f.get_data_window(1).unwrap_err().code, oor);
    let dw = f.get_data_window(0).unwrap();

    assert_eq!(f.read_scanline_chunk_info(-1, 42).unwrap_err().code, oor);
    assert_eq!(f.read_scanline_chunk_info(1, 42).unwrap_err().code, oor);

    assert_eq!(
        f.read_tile_chunk_info(0, 4, 2, 0, 0).unwrap_err().code,
        ErrorCode::TileScanMixedapi
    );

    let inval = ErrorCode::InvalidArgument;
    assert_eq!(
        f.read_scanline_chunk_info(0, dw.min.y - 1)
            .unwrap_err()
            .code,
        inval
    );
    assert_eq!(
        f.read_scanline_chunk_info(0, dw.max.y + 1)
            .unwrap_err()
            .code,
        inval
    );
    let cinfo = f.read_scanline_chunk_info(0, dw.min.y).unwrap();

    let pchunksz = f.get_chunk_unpacked_size(0).unwrap();
    assert_eq!(cinfo.type_, Storage::SCANLINE.0 as u8);
    assert_eq!(cinfo.compression, Compression::NONE.0 as u8);
    assert_eq!(cinfo.packed_size, pchunksz);
    assert_eq!(cinfo.unpacked_size, pchunksz);
    assert_eq!(cinfo.sample_count_data_offset, 0);
    assert_eq!(cinfo.sample_count_table_size, 0);

    let mut decoder = f.decoding_initialize(0, &cinfo).unwrap();

    assert_eq!(decoder.channel_count, 2);
    assert_eq!(decoder.channels[0].channel_name, b"R");
    assert_eq!(decoder.channels[0].bytes_per_element, 2);
    assert_eq!(decoder.channels[0].data_type, PixelType::HALF.0 as u16);
    assert_eq!(decoder.channels[0].width, 178);
    assert_eq!(decoder.channels[0].height, 1);
    assert_eq!(decoder.channels[0].x_samples, 1);
    assert_eq!(decoder.channels[0].y_samples, 1);
    assert_eq!(decoder.channels[1].channel_name, b"Z");
    assert_eq!(decoder.channels[1].bytes_per_element, 4);
    assert_eq!(decoder.channels[1].data_type, PixelType::FLOAT.0 as u16);
    assert_eq!(decoder.channels[1].width, 178);
    assert_eq!(decoder.channels[1].height, 1);
    assert_eq!(decoder.channels[1].x_samples, 1);
    assert_eq!(decoder.channels[1].y_samples, 1);

    let mut rptr = vec![0xFFu8; 178 * 2];
    let mut zptr = vec![0xFFu8; 178 * 4];
    decoder.channels[0].data = Some(BufferRef {
        buffer: 0,
        offset: 0,
    });
    decoder.channels[0].user_pixel_stride = 2;
    decoder.channels[0].user_line_stride = 2 * 178;
    decoder.channels[1].data = Some(BufferRef {
        buffer: 1,
        offset: 0,
    });
    decoder.channels[1].user_pixel_stride = 4;
    decoder.channels[1].user_line_stride = 4 * 178;

    f.decoding_choose_default_routines(0, &mut decoder).unwrap();

    f.decoding_run(0, &mut decoder, &mut [&mut rptr, &mut zptr])
        .unwrap();

    // it is compression: none
    assert!(decoder.packed_buffer.is_none());
    // it is compression: none
    assert!(decoder.unpacked_is_null());
    // TODO (upstream's): add actual comparison against C++ library
    let curr = u16::from_ne_bytes([rptr[0], rptr[1]]);
    let curz = f32::from_ne_bytes([zptr[0], zptr[1], zptr[2], zptr[3]]);
    assert_eq!(curr, 0);
    assert!((curz - 0.101991f32).abs() < 0.000001f32);

    f.decoding_destroy(&mut decoder).unwrap();

    let mut decoder = f.decoding_initialize(0, &cinfo).unwrap();
    let mut rptr = vec![0u8; 178 * 4];
    decoder.channels[0].data = Some(BufferRef {
        buffer: 0,
        offset: 0,
    });
    decoder.channels[0].user_pixel_stride = 4;
    decoder.channels[0].user_line_stride = 4 * 178;
    decoder.channels[0].user_bytes_per_element = 4;
    decoder.channels[0].user_data_type = PixelType::FLOAT.0 as u16;
    decoder.channels[1].data = Some(BufferRef {
        buffer: 1,
        offset: 0,
    });
    decoder.channels[1].user_pixel_stride = 4;
    decoder.channels[1].user_line_stride = 4 * 178;

    f.decoding_choose_default_routines(0, &mut decoder).unwrap();

    f.decoding_run(0, &mut decoder, &mut [&mut rptr, &mut zptr])
        .unwrap();

    f.decoding_destroy(&mut decoder).unwrap();
}

/// Port of `testReadTiles` (src/test/OpenEXRCoreTest/read.cpp:405-580) @ v3.5.3. Left out: the
/// NULL context and output calls (PLAN.md O8); `exr_get_tile_levels` and `exr_get_tile_sizes`
/// return both values, so the calls asking for one of them check that one.
#[test]
fn test_read_tiles() {
    let fn_ = test_image_dir() + "v1.7.test.tiled.exr";
    let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

    let ps = f.get_storage(0).unwrap();
    assert_eq!(Storage::TILED, ps);

    let oor = ErrorCode::ArgumentOutOfRange;
    assert_eq!(f.get_chunk_count(-1).unwrap_err().code, oor);
    assert_eq!(f.get_chunk_count(11).unwrap_err().code, oor);
    f.get_chunk_count(0).unwrap();
    assert_eq!(
        f.get_scanlines_per_chunk(0).unwrap_err().code,
        ErrorCode::ScanTileMixedapi
    );

    assert_eq!(f.get_tile_levels(1).unwrap_err().code, oor);
    let (levelsx, levelsy) = f.get_tile_levels(0).unwrap();
    assert_eq!(levelsx, 1);
    assert_eq!(levelsy, 1);

    assert_eq!(f.get_tile_sizes(0, -1, 0).unwrap_err().code, oor);
    assert_eq!(f.get_tile_sizes(0, 0, -1).unwrap_err().code, oor);
    assert_eq!(f.get_tile_sizes(0, 0, 100).unwrap_err().code, oor);
    assert_eq!(f.get_tile_sizes(0, 100, 0).unwrap_err().code, oor);
    let (levelsx, levelsy) = f.get_tile_sizes(0, 0, 0).unwrap();
    assert_eq!(levelsx, 12);
    assert_eq!(levelsy, 24);

    assert_eq!(
        f.read_scanline_chunk_info(0, 42).unwrap_err().code,
        ErrorCode::ScanTileMixedapi
    );

    assert_eq!(
        f.read_tile_chunk_info(-1, 4, 2, 0, 0).unwrap_err().code,
        oor
    );
    assert_eq!(f.read_tile_chunk_info(1, 4, 2, 0, 0).unwrap_err().code, oor);
    let inval = ErrorCode::InvalidArgument;
    assert_eq!(
        f.read_tile_chunk_info(0, 4, 2, 0, -1).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.read_tile_chunk_info(0, 4, 2, -1, 0).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.read_tile_chunk_info(0, 4, -2, 0, 0).unwrap_err().code,
        inval
    );
    assert_eq!(
        f.read_tile_chunk_info(0, -4, 2, 0, 0).unwrap_err().code,
        inval
    );

    // actually read a tile...
    let cinfo = f.read_tile_chunk_info(0, 4, 2, 0, 0).unwrap();
    let pchunksz = f.get_chunk_unpacked_size(0).unwrap();
    assert_eq!(cinfo.type_, Storage::TILED.0 as u8);
    assert_eq!(cinfo.compression, Compression::NONE.0 as u8);
    assert_eq!(cinfo.packed_size, pchunksz);
    assert_eq!(cinfo.unpacked_size, pchunksz);
    assert_eq!(cinfo.sample_count_data_offset, 0);
    assert_eq!(cinfo.sample_count_table_size, 0);

    let mut decoder = f.decoding_initialize(0, &cinfo).unwrap();

    assert_eq!(decoder.channel_count, 2);
    assert_eq!(decoder.channels[0].channel_name, b"G");
    assert_eq!(decoder.channels[0].bytes_per_element, 2);
    assert_eq!(decoder.channels[0].width, 12);
    assert_eq!(decoder.channels[0].height, 24);
    assert_eq!(decoder.channels[0].x_samples, 1);
    assert_eq!(decoder.channels[0].y_samples, 1);
    assert_eq!(decoder.channels[1].channel_name, b"Z");
    assert_eq!(decoder.channels[1].bytes_per_element, 4);
    assert_eq!(decoder.channels[1].width, 12);
    assert_eq!(decoder.channels[1].height, 24);
    assert_eq!(decoder.channels[1].x_samples, 1);
    assert_eq!(decoder.channels[1].y_samples, 1);

    let mut gptr = vec![0u8; 24 * 12 * 2];
    let mut zptr = vec![0u8; 24 * 12 * 4];
    decoder.channels[0].data = Some(BufferRef {
        buffer: 0,
        offset: 0,
    });
    decoder.channels[0].user_pixel_stride = 2;
    decoder.channels[0].user_line_stride = 2 * 12;
    decoder.channels[0].user_bytes_per_element = 2;
    decoder.channels[1].data = Some(BufferRef {
        buffer: 1,
        offset: 0,
    });
    decoder.channels[1].user_pixel_stride = 4;
    decoder.channels[1].user_line_stride = 4 * 12;
    decoder.channels[1].user_bytes_per_element = 4;

    f.decoding_choose_default_routines(0, &mut decoder).unwrap();

    f.decoding_run(0, &mut decoder, &mut [&mut gptr, &mut zptr])
        .unwrap();

    // it is compression: none
    assert!(decoder.packed_buffer.is_none());
    // it is compression: none
    assert!(decoder.unpacked_is_null());
    let curg = u16::from_ne_bytes([gptr[0], gptr[1]]);
    let curz = f32::from_ne_bytes([zptr[0], zptr[1], zptr[2], zptr[3]]);
    assert_eq!(curg, 0x33d5);
    assert!((curz - 0.244778f32).abs() < 0.000001f32);

    f.decoding_destroy(&mut decoder).unwrap();
}

/// Port of `testReadMultiPart` (src/test/OpenEXRCoreTest/read.cpp:582-584) @ v3.5.3: empty
/// upstream.
#[test]
fn test_read_multi_part() {}

/// Port of `testReadUnpack` (src/test/OpenEXRCoreTest/read.cpp:586-630) @ v3.5.3.
#[test]
fn test_read_unpack() {
    let fn_ = test_image_dir() + "v1.7.test.tiled.exr";
    let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

    let cinfo = f.read_tile_chunk_info(0, 4, 2, 0, 0).unwrap();

    {
        let mut decoder = f.decoding_initialize(0, &cinfo).unwrap();

        let mut gptr = vec![0u8; 24 * 12 * 4];
        let mut zptr = vec![0u8; 24 * 12 * 2];
        decoder.channels[0].data = Some(BufferRef {
            buffer: 0,
            offset: 0,
        });
        decoder.channels[0].user_pixel_stride = 4;
        decoder.channels[0].user_line_stride = 4 * 12;
        decoder.channels[0].user_bytes_per_element = 4;
        decoder.channels[0].user_data_type = PixelType::FLOAT.0 as u16;
        decoder.channels[1].data = Some(BufferRef {
            buffer: 1,
            offset: 0,
        });
        decoder.channels[1].user_pixel_stride = 2;
        decoder.channels[1].user_line_stride = 2 * 12;
        decoder.channels[1].user_bytes_per_element = 2;
        decoder.channels[1].user_data_type = PixelType::HALF.0 as u16;

        f.decoding_choose_default_routines(0, &mut decoder).unwrap();

        f.decoding_run(0, &mut decoder, &mut [&mut gptr, &mut zptr])
            .unwrap();

        f.decoding_destroy(&mut decoder).unwrap();
    }
}

/// Port of `hardway_height` (src/test/OpenEXRCoreTest/read.cpp:634-650 @ v3.5.3): the sampled
/// lines of `height` lines from `start_y`, counted one by one.
fn hardway_height(height: i32, y_sampling: i32, start_y: i32) -> i32 {
    let mut nlines = 0;
    let end = start_y + height;

    if y_sampling <= 1 {
        return height;
    }

    if height == 1 {
        return if start_y % y_sampling == 0 { 1 } else { 0 };
    }

    for y in start_y..end {
        if y % y_sampling != 0 {
            continue;
        }
        nlines += 1;
    }
    nlines
}

/// Port of `test_lpc` (src/test/OpenEXRCoreTest/read.cpp:652-675 @ v3.5.3).
fn test_lpc(samp: i32, h: i32, lpc: i32) {
    for y in -100..100 {
        let sy = y * samp;
        let ey = sy + h - 1;
        let mut ty = sy;
        while ty < ey {
            let mut th = ty + lpc;
            if th > ey {
                th = ey - ty + 1;
            } else {
                th = lpc;
            }

            let nl = crate::internal_util::compute_sampled_height(th, samp, ty);
            let hnl = hardway_height(th, samp, ty);
            assert_eq!(
                nl, hnl,
                "ty {ty} h {th} ({h}) samp {samp} lpc {lpc} => nl {nl} hnl {hnl}"
            );
            ty += lpc;
        }
    }
}

/// Port of `testSamplingCalcs` (src/test/OpenEXRCoreTest/read.cpp:677-694) @ v3.5.3.
#[test]
fn test_sampling_calcs() {
    let lines = [1, 16, 32, 256];

    for s in 2..11 {
        for lpc in lines {
            test_lpc(s, 1000, lpc);
        }
    }

    // sampling larger than the lines per chunk: most chunks hold no sampled row
    let bigsamp = [17, 33, 96, 257, 768];
    for s in bigsamp {
        for lpc in lines {
            test_lpc(s, 1000, lpc);
        }
    }
}
