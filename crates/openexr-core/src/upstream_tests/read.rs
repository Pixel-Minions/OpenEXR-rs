// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/read.cpp` @ v3.5.3.

use std::sync::Arc;

use crate::context::{Context, ErrorReport, Initializer};
use crate::errors::ErrorCode;

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
