// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Ports of `src/test/OpenEXRCoreTest/base_units.cpp` @ v3.5.3.

use crate::internal_coding::*;
use crate::upstream_tests::DEFAULTS;
use crate::version::OPENEXR_VERSION_HEX;
use crate::*;

/// `COMP_MAJ`, `COMP_MIN`, `COMP_PATCH`, `COMP_EXTRA`: what upstream's test build compiles in
/// (src/test/OpenEXRCoreTest/CMakeLists.txt:31-36 @ v3.5.3), from
/// src/lib/OpenEXRCore/openexr_version.h:11-13 and the default empty release type
/// (CMakeLists.txt:32) @ v3.5.3.
const COMP: (i32, i32, i32, &str) = (3, 5, 3, "");

/// Port of `testBase` (src/test/OpenEXRCoreTest/base_units.cpp:29-70) @ v3.5.3.
#[test]
fn test_base() {
    let (major, minor, patch, extra) = get_library_version();
    assert_eq!((major, minor, patch, extra), COMP, "wrong library version");

    let major = (OPENEXR_VERSION_HEX >> 24) & 0xff;
    let minor = (OPENEXR_VERSION_HEX >> 16) & 0xff;
    let patch = (OPENEXR_VERSION_HEX >> 8) & 0xff;
    assert_eq!(major, COMP.0);
    assert_eq!(minor, COMP.1);
    assert_eq!(patch, COMP.2);
    // Upstream confirms the macro compiles in an `#if`; the Rust counterpart is a const check.
    const { assert!(OPENEXR_VERSION_HEX > 0) };
}

/// Port of `testBaseErrors` (src/test/OpenEXRCoreTest/base_units.cpp:72-247) @ v3.5.3.
#[test]
fn test_base_errors() {
    // Upstream checks only that these are not NULL, which a `&str` always is.
    get_default_error_message(ErrorCode::Success.code());
    get_default_error_message(ErrorCode::OutOfMemory.code());
    get_default_error_message(ErrorCode::Unknown.code());
    assert_eq!(get_default_error_message(-1), "Unknown error code");
    get_default_error_message(ErrorCode::Unknown.code() + 1);
    assert_eq!(get_default_error_message(110), "Unknown error code");

    assert_eq!(
        get_error_code_as_string(ErrorCode::Success.code()),
        "EXR_ERR_SUCCESS"
    );
    assert_eq!(
        get_error_code_as_string(ErrorCode::Unknown.code()),
        "EXR_ERR_UNKNOWN"
    );
    assert_eq!(
        get_error_code_as_string(ErrorCode::Unknown.code() + 1),
        "EXR_ERR_UNKNOWN"
    );
    assert_eq!(get_error_code_as_string(-1), "EXR_ERR_UNKNOWN");
    assert_eq!(get_error_code_as_string(-2), "EXR_ERR_UNKNOWN");
    assert_eq!(get_error_code_as_string(i32::MIN), "EXR_ERR_UNKNOWN");

    for (code, name) in [
        (ErrorCode::MissingReqAttr, "EXR_ERR_MISSING_REQ_ATTR"),
        (ErrorCode::OutOfMemory, "EXR_ERR_OUT_OF_MEMORY"),
        (ErrorCode::MissingContextArg, "EXR_ERR_MISSING_CONTEXT_ARG"),
        (ErrorCode::InvalidArgument, "EXR_ERR_INVALID_ARGUMENT"),
        (
            ErrorCode::ArgumentOutOfRange,
            "EXR_ERR_ARGUMENT_OUT_OF_RANGE",
        ),
        (ErrorCode::FileAccess, "EXR_ERR_FILE_ACCESS"),
        (ErrorCode::FileBadHeader, "EXR_ERR_FILE_BAD_HEADER"),
        (ErrorCode::NotOpenRead, "EXR_ERR_NOT_OPEN_READ"),
        (ErrorCode::NotOpenWrite, "EXR_ERR_NOT_OPEN_WRITE"),
        (ErrorCode::HeaderNotWritten, "EXR_ERR_HEADER_NOT_WRITTEN"),
        (ErrorCode::ReadIo, "EXR_ERR_READ_IO"),
        (ErrorCode::WriteIo, "EXR_ERR_WRITE_IO"),
        (ErrorCode::NameTooLong, "EXR_ERR_NAME_TOO_LONG"),
        (ErrorCode::MissingReqAttr, "EXR_ERR_MISSING_REQ_ATTR"),
        (ErrorCode::InvalidAttr, "EXR_ERR_INVALID_ATTR"),
        (ErrorCode::NoAttrByName, "EXR_ERR_NO_ATTR_BY_NAME"),
        (ErrorCode::BadChunkLeader, "EXR_ERR_BAD_CHUNK_LEADER"),
        (ErrorCode::CorruptChunk, "EXR_ERR_CORRUPT_CHUNK"),
        (ErrorCode::InvalidSampleData, "EXR_ERR_INVALID_SAMPLE_DATA"),
        (ErrorCode::AttrTypeMismatch, "EXR_ERR_ATTR_TYPE_MISMATCH"),
        (ErrorCode::AttrSizeMismatch, "EXR_ERR_ATTR_SIZE_MISMATCH"),
        (
            ErrorCode::IncompleteChunkTable,
            "EXR_ERR_INCOMPLETE_CHUNK_TABLE",
        ),
        (ErrorCode::ScanTileMixedapi, "EXR_ERR_SCAN_TILE_MIXEDAPI"),
        (ErrorCode::TileScanMixedapi, "EXR_ERR_TILE_SCAN_MIXEDAPI"),
        (ErrorCode::ModifySizeChange, "EXR_ERR_MODIFY_SIZE_CHANGE"),
        (ErrorCode::AlreadyWroteAttrs, "EXR_ERR_ALREADY_WROTE_ATTRS"),
        (ErrorCode::IncorrectPart, "EXR_ERR_INCORRECT_PART"),
        (ErrorCode::IncorrectChunk, "EXR_ERR_INCORRECT_CHUNK"),
        (ErrorCode::UseScanDeepWrite, "EXR_ERR_USE_SCAN_DEEP_WRITE"),
        (ErrorCode::UseTileDeepWrite, "EXR_ERR_USE_TILE_DEEP_WRITE"),
        (
            ErrorCode::UseScanNondeepWrite,
            "EXR_ERR_USE_SCAN_NONDEEP_WRITE",
        ),
        (
            ErrorCode::UseTileNondeepWrite,
            "EXR_ERR_USE_TILE_NONDEEP_WRITE",
        ),
    ] {
        assert_eq!(get_error_code_as_string(code.code()), name);
    }
}

/// Port of `testBaseLimits` (src/test/OpenEXRCoreTest/base_units.cpp:249-372) @ v3.5.3.
#[test]
fn test_base_limits() {
    let _guard = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());

    set_default_maximum_image_size(42, 42);
    assert_eq!(
        get_default_maximum_image_size(),
        (42, 42),
        "Unable to set_default_maximum_image_size"
    );
    for (w, h) in [(-1, -1), (84, -1), (-1, 84)] {
        set_default_maximum_image_size(w, h);
        assert_eq!(
            get_default_maximum_image_size(),
            (42, 42),
            "Invalid request not ignored"
        );
    }

    set_default_maximum_tile_size(128, 128);
    assert_eq!(
        get_default_maximum_tile_size(),
        (128, 128),
        "Unable to set_default_maximum_tile_size"
    );
    for (w, h) in [(-1, -1), (84, -1), (-1, 84)] {
        set_default_maximum_tile_size(w, h);
        assert_eq!(
            get_default_maximum_tile_size(),
            (128, 128),
            "Invalid request not ignored"
        );
    }
    set_default_maximum_image_size(0, 0);
    set_default_maximum_tile_size(0, 0);

    set_default_zip_compression_level(4);
    assert_eq!(get_default_zip_compression_level(), 4);
    set_default_zip_compression_level(-1);
    assert_eq!(get_default_zip_compression_level(), -1);
    set_default_zip_compression_level(-2);
    assert_eq!(get_default_zip_compression_level(), -1);
    set_default_zip_compression_level(15);
    assert_eq!(get_default_zip_compression_level(), 9);
    set_default_zip_compression_level(-1);

    set_default_dwa_compression_quality(23.0);
    assert_eq!(get_default_dwa_compression_quality(), 23.0);
    set_default_dwa_compression_quality(-1.0);
    assert_eq!(get_default_dwa_compression_quality(), 0.0);
    set_default_dwa_compression_quality(200.0);
    assert_eq!(get_default_dwa_compression_quality(), 100.0);
    set_default_dwa_compression_quality(45.0);

    set_default_lossy_htj2k_quality(75.0);
    assert_eq!(get_default_lossy_htj2k_quality(), 75.0);
    set_default_lossy_htj2k_quality(-1.0);
    assert_eq!(get_default_lossy_htj2k_quality(), 1.0);
    set_default_lossy_htj2k_quality(200.0);
    assert_eq!(get_default_lossy_htj2k_quality(), 150.0);
    set_default_lossy_htj2k_quality(110.0);
}

/// Port of `testHalf` (src/test/OpenEXRCoreTest/base_units.cpp:424-442) @ v3.5.3.
#[test]
fn test_half() {
    assert_eq!(half_to_float(0), 0.0);
    assert_eq!(float_to_half(0.0), 0);
    assert_eq!(float_to_half_int(0), 0);
    assert_eq!(half_to_float_int(0), 0);
    assert_eq!(half_to_uint(0), 0);
    assert_eq!(half_to_uint(0x8000), 0);
    assert_eq!(float_to_uint(0.0), 0);
    assert_eq!(float_to_uint(-1.0), 0);
    assert_eq!(float_to_uint_int(0), 0);

    assert_eq!(uint_to_half(0), 0);
    assert_eq!(uint_to_half(128344), 0x7c00);

    assert_eq!(uint_to_float(0), 0.0);
    assert_eq!(uint_to_float_int(0), 0);
}

/// Port of `testCPUIdent` (src/test/OpenEXRCoreTest/base_units.cpp:385-422) @ v3.5.3.
///
/// Upstream compares Core's check with the C++ library's `CpuId`
/// (src/lib/OpenEXR/ImfSystemSpecific.cpp:81-140 @ v3.5.3). The reference here is that class's
/// rule applied to the platform's own answers: under MSVC its `xgetbv` is a stub returning 0
/// (ImfSystemSpecific.cpp:69-77), so AVX and F16C are absent; elsewhere AVX and F16C need the
/// CPU bit plus OS support for AVX state, which Rust's `avx` detection reports.
#[test]
fn test_cpu_ident() {
    let id = crate::internal_cpuid::check_for_x86_simd();
    #[cfg(target_arch = "x86_64")]
    {
        let sse2 = std::arch::is_x86_feature_detected!("sse2");
        let (avx, f16c) = if cfg!(target_env = "msvc") {
            (false, false)
        } else {
            let avx = std::arch::is_x86_feature_detected!("avx");
            (avx, avx && std::arch::is_x86_feature_detected!("f16c"))
        };
        assert_eq!(id.f16c, f16c as i32, "CPU Id test f16c mismatch");
        assert_eq!(id.avx, avx as i32, "CPU Id test avx mismatch");
        assert_eq!(id.sse2, sse2 as i32, "CPU Id test sse2 mismatch");
        assert_eq!(
            crate::internal_cpuid::has_native_half(),
            (id.f16c != 0 && id.avx != 0) as i32,
            "CPU Id test has native half mismatch"
        );
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = id;
        crate::internal_cpuid::has_native_half();
    }
}

/// Partial port of `testTempContext` (src/test/OpenEXRCoreTest/base_units.cpp:446-470 @ v3.5.3):
/// the first context. The call with a NULL out-pointer has no Rust counterpart, and the second
/// context needs `exr_initialize_required_attr_simple` (Phase 3), which completes this port.
#[test]
fn test_temp_context_first_context() {
    let _guard = DEFAULTS.lock().unwrap_or_else(|e| e.into_inner());
    let tempdir = std::env::temp_dir();
    let name = tempdir.to_string_lossy();
    let c = Context::start_temporary_context(Some(&name), None).expect("start_temporary_context");
    assert_eq!(c.get_count(), 1);
    c.finish().expect("finish");
}
