// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/deep.cpp` @ v3.5.3.
//!
//! Upstream's `testOpenDeep` and `testReadDeep` write their deep files with the C++ library
//! (`generateRandomScanFile`, `generateRandomTileFile`); the wheel stands in for it
//! (`oracle/write_deep.py`), so they run in the heavy tier.

use std::sync::Arc;

use crate::context::{Context, ErrorReport, Initializer};
use crate::decoding::EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL;
use crate::errors::ErrorCode;
use crate::upstream_tests::read::make_temp_dir;

/// `err_cb` (src/test/OpenEXRCoreTest/deep.cpp:28-32 @ v3.5.3).
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

/// `width`, `height`, `minY` (src/test/OpenEXRCoreTest/deep.cpp:35-38 @ v3.5.3).
#[cfg(feature = "heavy")]
const WIDTH: i32 = 273;
#[cfg(feature = "heavy")]
const HEIGHT: i32 = 169;
#[cfg(feature = "heavy")]
const MIN_Y: i32 = 11;

/// `generateRandomScanFile` and `generateRandomTileFile`
/// (src/test/OpenEXRCoreTest/deep.cpp:48-370 @ v3.5.3), done by the wheel
/// (`oracle/write_deep.py`), seeded by the file's place in the test's loops.
#[cfg(feature = "heavy")]
fn generate(fn_: &str, tiled: bool, channel_count: i32, compression: &str, seed: u32) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = std::process::Command::new("uv")
        .arg("run")
        .arg("--project")
        .arg(root.join("oracle"))
        .arg("python")
        .arg("-I")
        .arg(root.join("oracle/write_deep.py"))
        .arg(fn_)
        .arg(if tiled { "tile" } else { "scan" })
        .arg(channel_count.to_string())
        .arg(compression)
        .arg(seed.to_string())
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "write_deep.py failed");
}

/// The compressions of upstream's loops, with the wheel's names for them.
#[cfg(feature = "heavy")]
const COMPS: [&str; 3] = ["NO_COMPRESSION", "RLE_COMPRESSION", "ZIPS_COMPRESSION"];

/// Port of `testOpenDeep` (src/test/OpenEXRCoreTest/deep.cpp:372-408) @ v3.5.3, the files
/// written by the wheel; its check that the C++ library opens them is the wheel's writing
/// them.
#[cfg(feature = "heavy")]
#[test]
fn test_open_deep() {
    let tempdir = make_temp_dir();
    let fn_ = format!("{tempdir}randomtempdeep.exr");

    let chancounts = [1, 3, 10];
    let mut seed = 0;
    for &c in &chancounts {
        for comp in COMPS {
            seed += 1;
            generate(&fn_, false, c, comp, seed);
            let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");
            f.finish().expect("exr_finish");
            generate(&fn_, true, c, comp, seed);
            let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");
            f.finish().expect("exr_finish");
        }
    }
    let _ = std::fs::remove_file(&fn_);
}

/// Port of `testReadDeep` (src/test/OpenEXRCoreTest/deep.cpp:410-498) @ v3.5.3, the files
/// written by the wheel.
#[cfg(feature = "heavy")]
#[test]
fn test_read_deep() {
    use crate::openexr_attr::PixelType;

    let tempdir = make_temp_dir();
    let fn_ = format!("{tempdir}randomtempdeep.exr");

    let chancounts = [1, 3, 10];
    let mut seed = 0;
    for &c in &chancounts {
        for (cp, comp) in COMPS.iter().enumerate() {
            seed += 1;
            generate(&fn_, false, c, comp, seed);
            let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

            let cinfo = f
                .read_scanline_chunk_info(0, MIN_Y + HEIGHT / 2)
                .expect("chunk info");
            let mut packed = vec![0u8; cinfo.packed_size as usize];
            let mut sampdata = vec![0u8; cinfo.sample_count_table_size as usize];
            f.read_deep_chunk(0, &cinfo, Some(&mut packed), Some(&mut sampdata))
                .expect("exr_read_deep_chunk");
            if cp == 0 {
                // The table is returned as stored in the file, i.e. little-endian, so decode
                // it explicitly rather than reinterpreting it as host-order integers.
                let n = sampdata.len() / 4;
                let le32 = |i: usize| {
                    u32::from_le_bytes([
                        sampdata[i * 4],
                        sampdata[i * 4 + 1],
                        sampdata[i * 4 + 2],
                        sampdata[i * 4 + 3],
                    ])
                };
                assert_eq!(n, WIDTH as usize);
                // channelTypes: the file's channels, in its order
                let mut bps = 0usize;
                for ch in &f.get_channels(0).expect("channels").entries {
                    if ch.pixel_type == PixelType::UINT {
                        bps += 4;
                    }
                    if ch.pixel_type == PixelType::HALF {
                        bps += 2;
                    }
                    if ch.pixel_type == PixelType::FLOAT {
                        bps += 4;
                    }
                }
                assert_eq!(packed.len(), le32(n - 1) as usize * bps);
            }

            let cinfo = f
                .read_scanline_chunk_info(0, MIN_Y + HEIGHT / 4)
                .expect("chunk info");
            let mut packed = vec![0u8; cinfo.packed_size as usize];
            let mut sampdata = vec![0u8; cinfo.sample_count_table_size as usize];
            // we support reading the two bits separately
            f.read_deep_chunk(0, &cinfo, Some(&mut packed), None)
                .expect("exr_read_deep_chunk");
            f.read_deep_chunk(0, &cinfo, None, Some(&mut sampdata))
                .expect("exr_read_deep_chunk");

            f.finish().expect("exr_finish");

            generate(&fn_, true, c, comp, seed);
            let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

            let cinfo = f
                .read_tile_chunk_info(0, 0, 1, 0, 0)
                .expect("tile chunk info");
            let mut packed = vec![0u8; cinfo.packed_size as usize];
            let mut sampdata = vec![0u8; cinfo.sample_count_table_size as usize];
            f.read_deep_chunk(0, &cinfo, Some(&mut packed), Some(&mut sampdata))
                .expect("exr_read_deep_chunk");

            f.finish().expect("exr_finish");
        }
    }
    let _ = std::fs::remove_file(&fn_);
}

/// Port of `testWriteDeep` (src/test/OpenEXRCoreTest/deep.cpp:500-503) @ v3.5.3: empty
/// upstream.
#[test]
fn test_write_deep() {}

/// `deepZstdTileData` (src/test/OpenEXRCoreTest/deep.cpp:525-567 @ v3.5.3): a single 1x3
/// deep tile (channels A half, B float), ZSTD compressed, whose 12-byte sample count table
/// (100, 10, -10) is stored uncompressed before the 52-byte ZSTD payload at the end of the file.
#[rustfmt::skip]
pub(crate) const DEEP_ZSTD_TILE_DATA: [u8; 502] = [
    0x76, 0x2f, 0x31, 0x01, 0x02, 0x08, 0x00, 0x00, 0x63, 0x68, 0x61, 0x6e,
    0x6e, 0x65, 0x6c, 0x73, 0x00, 0x63, 0x68, 0x6c, 0x69, 0x73, 0x74, 0x00,
    0x25, 0x00, 0x00, 0x00, 0x41, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x42, 0x00,
    0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x00, 0x63, 0x68, 0x75, 0x6e, 0x6b, 0x43, 0x6f,
    0x75, 0x6e, 0x74, 0x00, 0x69, 0x6e, 0x74, 0x00, 0x04, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x63, 0x6f, 0x6d, 0x70, 0x72, 0x65, 0x73, 0x73,
    0x69, 0x6f, 0x6e, 0x00, 0x63, 0x6f, 0x6d, 0x70, 0x72, 0x65, 0x73, 0x73,
    0x69, 0x6f, 0x6e, 0x00, 0x01, 0x00, 0x00, 0x00, 0x0d, 0x64, 0x61, 0x74,
    0x61, 0x57, 0x69, 0x6e, 0x64, 0x6f, 0x77, 0x00, 0x62, 0x6f, 0x78, 0x32,
    0x69, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x64, 0x69,
    0x73, 0x70, 0x6c, 0x61, 0x79, 0x57, 0x69, 0x6e, 0x64, 0x6f, 0x77, 0x00,
    0x62, 0x6f, 0x78, 0x32, 0x69, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x00,
    0x00, 0x00, 0x6c, 0x69, 0x6e, 0x65, 0x4f, 0x72, 0x64, 0x65, 0x72, 0x00,
    0x6c, 0x69, 0x6e, 0x65, 0x4f, 0x72, 0x64, 0x65, 0x72, 0x00, 0x01, 0x00,
    0x00, 0x00, 0x00, 0x70, 0x69, 0x78, 0x65, 0x6c, 0x41, 0x73, 0x70, 0x65,
    0x63, 0x74, 0x52, 0x61, 0x74, 0x69, 0x6f, 0x00, 0x66, 0x6c, 0x6f, 0x61,
    0x74, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x3f, 0x73, 0x63,
    0x72, 0x65, 0x65, 0x6e, 0x57, 0x69, 0x6e, 0x64, 0x6f, 0x77, 0x43, 0x65,
    0x6e, 0x74, 0x65, 0x72, 0x00, 0x76, 0x32, 0x66, 0x00, 0x08, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x73, 0x63, 0x72,
    0x65, 0x65, 0x6e, 0x57, 0x69, 0x6e, 0x64, 0x6f, 0x77, 0x57, 0x69, 0x64,
    0x74, 0x68, 0x00, 0x66, 0x6c, 0x6f, 0x61, 0x74, 0x00, 0x04, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x80, 0x3f, 0x74, 0x69, 0x6c, 0x65, 0x73, 0x00, 0x74,
    0x69, 0x6c, 0x65, 0x64, 0x65, 0x73, 0x63, 0x00, 0x09, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x74, 0x79, 0x70,
    0x65, 0x00, 0x73, 0x74, 0x72, 0x69, 0x6e, 0x67, 0x00, 0x08, 0x00, 0x00,
    0x00, 0x64, 0x65, 0x65, 0x70, 0x74, 0x69, 0x6c, 0x65, 0x76, 0x65, 0x72,
    0x73, 0x69, 0x6f, 0x6e, 0x00, 0x69, 0x6e, 0x74, 0x00, 0x04, 0x00, 0x00,
    0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x8e, 0x01, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0c, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x34, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x58, 0x02,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x64, 0x00, 0x00, 0x00, 0x0a, 0x00,
    0x00, 0x00, 0xf6, 0xff, 0xff, 0xff, 0x7a, 0x73, 0x74, 0x64, 0x2d, 0x65,
    0x78, 0x72, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1c, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x58,
    0x9d, 0x00, 0x00, 0x38, 0xc8, 0x00, 0x41, 0x90, 0x01, 0x00, 0x42, 0x04,
    0x10, 0x00, 0x8c, 0x4a, 0x08, 0x43, 0xa4, 0x77, 0xc0, 0x02,
];

/// Port of `decodeDeepZstdTile` (src/test/OpenEXRCoreTest/deep.cpp:509-605 @ v3.5.3): the
/// fixture with its sample count table replaced by `counts`, its tile decoded with the extra
/// `decode_flags`; the result of `exr_decoding_run`.
fn decode_deep_zstd_tile(
    tempdir: &str,
    counts: [i32; 3],
    decode_flags: u16,
) -> Result<(), crate::context::Error> {
    let mut data = DEEP_ZSTD_TILE_DATA.to_vec();
    let tbl = data.len() - 64;
    assert!(data[tbl] == 0x64 && data[tbl + 8] == 0xf6);
    for (i, c) in counts.iter().enumerate() {
        data[tbl + i * 4..tbl + i * 4 + 4].copy_from_slice(&c.to_le_bytes());
    }

    let fn_ = format!("{tempdir}deepzstdsamplecounts.exr");
    std::fs::write(&fn_, &data).expect("fwrite");

    let f = Context::start_read(Some(&fn_), cinit()).expect("exr_start_read");

    let cinfo = f
        .read_tile_chunk_info(0, 0, 0, 0, 0)
        .expect("exr_read_tile_chunk_info");
    let mut decoder = f
        .decoding_initialize(0, &cinfo)
        .expect("exr_decoding_initialize");
    decoder.decode_flags |= decode_flags;
    f.decoding_choose_default_routines(0, &mut decoder)
        .expect("exr_decoding_choose_default_routines");
    let rv = f.decoding_run(0, &mut decoder, &mut []);
    f.decoding_destroy(&mut decoder)
        .expect("exr_decoding_destroy");
    f.finish().expect("exr_finish");

    let _ = std::fs::remove_file(&fn_);
    rv
}

/// Port of `testDeepZstdNegativeSampleCounts` (src/test/OpenEXRCoreTest/deep.cpp:611-626)
/// @ v3.5.3: negative sample counts are rejected before the pixel data is unsorted.
#[test]
fn test_deep_zstd_negative_sample_counts() {
    let tempdir = make_temp_dir();
    let counts = [100, 10, -10];

    assert_eq!(
        decode_deep_zstd_tile(&tempdir, counts, 0).unwrap_err().code,
        ErrorCode::InvalidSampleData
    );
    assert_eq!(
        decode_deep_zstd_tile(&tempdir, counts, EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL)
            .unwrap_err()
            .code,
        ErrorCode::InvalidSampleData
    );
}

/// Port of `testDeepZstdValidSampleCounts` (src/test/OpenEXRCoreTest/deep.cpp:628-639)
/// @ v3.5.3: a valid chunk whose sample count table is stored uncompressed decodes.
#[test]
fn test_deep_zstd_valid_sample_counts() {
    let tempdir = make_temp_dir();
    let counts = [50, 25, 25];

    assert!(decode_deep_zstd_tile(&tempdir, counts, 0).is_ok());
    assert!(
        decode_deep_zstd_tile(&tempdir, counts, EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL).is_ok()
    );
}
