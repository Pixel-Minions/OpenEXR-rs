// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `base.c` (src/lib/OpenEXRCore/base.c:1-251 @ v3.5.3): the library version, the error
//! code names and messages, and the process-wide defaults new contexts start from.
//!
//! Upstream keeps the defaults in unsynchronized `static` variables; here they are relaxed
//! atomics, which load and store the same values without data races. As in C, a width and
//! height set together are two separate stores.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

use crate::errors::ExrResult;
use crate::internal_ht_quality::clamp_lossy_htj2k_quality;
use crate::version::{
    OPENEXR_VERSION_EXTRA, OPENEXR_VERSION_MAJOR, OPENEXR_VERSION_MINOR, OPENEXR_VERSION_PATCH,
};

/// Port of `exr_get_library_version` (src/lib/OpenEXRCore/base.c:14-25 @ v3.5.3).
/// Returns `(major, minor, patch, extra)`; the C out-pointers are each optional, which a tuple
/// makes moot.
pub fn get_library_version() -> (i32, i32, i32, &'static str) {
    (
        OPENEXR_VERSION_MAJOR,
        OPENEXR_VERSION_MINOR,
        OPENEXR_VERSION_PATCH,
        OPENEXR_VERSION_EXTRA,
    )
}

/// `the_error_code_names` (src/lib/OpenEXRCore/base.c:29-63 @ v3.5.3). Upstream's table has no
/// entries for `EXR_ERR_COMPRESSION_FAILED` and `EXR_ERR_DECOMPRESSION_FAILED`, so it has 34
/// entries for 36 codes (docs/improvements.md, I-1).
static THE_ERROR_CODE_NAMES: [&str; 34] = [
    "EXR_ERR_SUCCESS",
    "EXR_ERR_OUT_OF_MEMORY",
    "EXR_ERR_MISSING_CONTEXT_ARG",
    "EXR_ERR_INVALID_ARGUMENT",
    "EXR_ERR_ARGUMENT_OUT_OF_RANGE",
    "EXR_ERR_FILE_ACCESS",
    "EXR_ERR_FILE_BAD_HEADER",
    "EXR_ERR_NOT_OPEN_READ",
    "EXR_ERR_NOT_OPEN_WRITE",
    "EXR_ERR_HEADER_NOT_WRITTEN",
    "EXR_ERR_READ_IO",
    "EXR_ERR_WRITE_IO",
    "EXR_ERR_NAME_TOO_LONG",
    "EXR_ERR_MISSING_REQ_ATTR",
    "EXR_ERR_INVALID_ATTR",
    "EXR_ERR_NO_ATTR_BY_NAME",
    "EXR_ERR_ATTR_TYPE_MISMATCH",
    "EXR_ERR_ATTR_SIZE_MISMATCH",
    "EXR_ERR_SCAN_TILE_MIXEDAPI",
    "EXR_ERR_TILE_SCAN_MIXEDAPI",
    "EXR_ERR_MODIFY_SIZE_CHANGE",
    "EXR_ERR_ALREADY_WROTE_ATTRS",
    "EXR_ERR_BAD_CHUNK_LEADER",
    "EXR_ERR_CORRUPT_CHUNK",
    "EXR_ERR_INCOMPLETE_CHUNK_TABLE",
    "EXR_ERR_INCORRECT_PART",
    "EXR_ERR_INCORRECT_CHUNK",
    "EXR_ERR_USE_SCAN_DEEP_WRITE",
    "EXR_ERR_USE_TILE_DEEP_WRITE",
    "EXR_ERR_USE_SCAN_NONDEEP_WRITE",
    "EXR_ERR_USE_TILE_NONDEEP_WRITE",
    "EXR_ERR_INVALID_SAMPLE_DATA",
    "EXR_ERR_FEATURE_NOT_IMPLEMENTED",
    "EXR_ERR_UNKNOWN",
];

/// `the_default_errors` (src/lib/OpenEXRCore/base.c:69-105 @ v3.5.3).
static THE_DEFAULT_ERRORS: [&str; 36] = [
    "Success",
    "Unable to allocate memory",
    "Context argument to function is not valid",
    "Invalid argument to function",
    "Argument to function out of valid range",
    "Unable to open file (path does not exist or permission denied)",
    "File is not an OpenEXR file or has a bad header value",
    "File not opened for read",
    "File not opened for write",
    "File opened for write, but header not yet written",
    "Error reading from stream",
    "Error writing to stream",
    "Text too long for file flags",
    "Missing required attribute in part header",
    "Invalid attribute in part header",
    "No attribute by that name in part header",
    "Attribute type mismatch",
    "Attribute type vs. size mismatch",
    "Attempt to use a scanline accessor function for a tiled image",
    "Attempt to use a tiled accessor function for a scanline image",
    "Attempt to modify a value when in update mode with different size",
    "File in write mode, but header already written, can no longer edit attributes",
    "Unexpected or corrupt values in data block leader vs computed value",
    "Corrupt data block data, unable to decode",
    "Chunk offsets table not completely finished writing (incomplete file)",
    "Previous part not yet finished writing",
    "Invalid data block to write at this point",
    "Use deep scanline write with the sample count table arguments",
    "Use deep tile write with the sample count table arguments",
    "Use non-deep scanline write (sample count table invalid for this part type)",
    "Use non-deep tile write (sample count table invalid for this part type)",
    "Invalid sample data table value",
    "Feature not yet implemented, please use C++ library",
    "Chunk compression failed",
    "Chunk decompression failed",
    "Unknown error code",
];

/// The shared index rule of the two lookups: out of range means the table's last entry.
fn table_index(code: ExrResult, count: usize) -> usize {
    match usize::try_from(code) {
        Ok(idx) if idx < count => idx,
        _ => count - 1,
    }
}

/// Port of `exr_get_default_error_message` (src/lib/OpenEXRCore/base.c:111-118 @ v3.5.3).
pub fn get_default_error_message(code: ExrResult) -> &'static str {
    THE_DEFAULT_ERRORS[table_index(code, THE_DEFAULT_ERRORS.len())]
}

/// Port of `exr_get_error_code_as_string` (src/lib/OpenEXRCore/base.c:122-128 @ v3.5.3).
pub fn get_error_code_as_string(code: ExrResult) -> &'static str {
    THE_ERROR_CODE_NAMES[table_index(code, THE_ERROR_CODE_NAMES.len())]
}

/// `sMaxW`, `sMaxH` (src/lib/OpenEXRCore/base.c:132-133 @ v3.5.3).
static S_MAX_W: AtomicI32 = AtomicI32::new(0);
static S_MAX_H: AtomicI32 = AtomicI32::new(0);

/// Port of `exr_set_default_maximum_image_size` (src/lib/OpenEXRCore/base.c:135-143 @ v3.5.3).
pub fn set_default_maximum_image_size(w: i32, h: i32) {
    if w >= 0 && h >= 0 {
        S_MAX_W.store(w, Ordering::Relaxed);
        S_MAX_H.store(h, Ordering::Relaxed);
    }
}

/// Port of `exr_get_default_maximum_image_size` (src/lib/OpenEXRCore/base.c:147-152 @ v3.5.3).
/// Returns `(w, h)`.
pub fn get_default_maximum_image_size() -> (i32, i32) {
    (
        S_MAX_W.load(Ordering::Relaxed),
        S_MAX_H.load(Ordering::Relaxed),
    )
}

/// `sTileMaxW`, `sTileMaxH` (src/lib/OpenEXRCore/base.c:156-157 @ v3.5.3).
static S_TILE_MAX_W: AtomicI32 = AtomicI32::new(0);
static S_TILE_MAX_H: AtomicI32 = AtomicI32::new(0);

/// Port of `exr_set_default_maximum_tile_size` (src/lib/OpenEXRCore/base.c:159-167 @ v3.5.3).
pub fn set_default_maximum_tile_size(w: i32, h: i32) {
    if w >= 0 && h >= 0 {
        S_TILE_MAX_W.store(w, Ordering::Relaxed);
        S_TILE_MAX_H.store(h, Ordering::Relaxed);
    }
}

/// Port of `exr_get_default_maximum_tile_size` (src/lib/OpenEXRCore/base.c:171-176 @ v3.5.3).
/// Returns `(w, h)`.
pub fn get_default_maximum_tile_size() -> (i32, i32) {
    (
        S_TILE_MAX_W.load(Ordering::Relaxed),
        S_TILE_MAX_H.load(Ordering::Relaxed),
    )
}

/// `sDefaultZipLevel` (src/lib/OpenEXRCore/base.c:180 @ v3.5.3).
static S_DEFAULT_ZIP_LEVEL: AtomicI32 = AtomicI32::new(-1);

/// Port of `exr_set_default_zip_compression_level` (src/lib/OpenEXRCore/base.c:182-188 @ v3.5.3).
pub fn set_default_zip_compression_level(mut l: i32) {
    if l < 0 {
        l = -1;
    }
    if l > 9 {
        l = 9;
    }
    S_DEFAULT_ZIP_LEVEL.store(l, Ordering::Relaxed);
}

/// Port of `exr_get_default_zip_compression_level` (src/lib/OpenEXRCore/base.c:192-196 @ v3.5.3).
pub fn get_default_zip_compression_level() -> i32 {
    S_DEFAULT_ZIP_LEVEL.load(Ordering::Relaxed)
}

/// `sDefaultDwaLevel` (src/lib/OpenEXRCore/base.c:200 @ v3.5.3), as the bits of the `float`.
static S_DEFAULT_DWA_LEVEL: AtomicU32 = AtomicU32::new(45.0f32.to_bits());

/// Port of `exr_set_default_dwa_compression_quality` (src/lib/OpenEXRCore/base.c:202-208 @ v3.5.3).
/// NaN fails both comparisons and is stored, as in C.
pub fn set_default_dwa_compression_quality(mut q: f32) {
    if q < 0.0 {
        q = 0.0;
    }
    if q > 100.0 {
        q = 100.0;
    }
    S_DEFAULT_DWA_LEVEL.store(q.to_bits(), Ordering::Relaxed);
}

/// Port of `exr_get_default_dwa_compression_quality` (src/lib/OpenEXRCore/base.c:212-216 @ v3.5.3).
pub fn get_default_dwa_compression_quality() -> f32 {
    f32::from_bits(S_DEFAULT_DWA_LEVEL.load(Ordering::Relaxed))
}

/// `sDefaultJ2kQuality` (src/lib/OpenEXRCore/base.c:220 @ v3.5.3), as the bits of the `float`.
static S_DEFAULT_J2K_QUALITY: AtomicU32 = AtomicU32::new(110.0f32.to_bits());

/// Port of `exr_set_default_lossy_htj2k_quality` (src/lib/OpenEXRCore/base.c:222-226 @ v3.5.3).
pub fn set_default_lossy_htj2k_quality(q: f32) {
    S_DEFAULT_J2K_QUALITY.store(clamp_lossy_htj2k_quality(q).to_bits(), Ordering::Relaxed);
}

/// Port of `exr_get_default_lossy_htj2k_quality` (src/lib/OpenEXRCore/base.c:230-234 @ v3.5.3).
pub fn get_default_lossy_htj2k_quality() -> f32 {
    f32::from_bits(S_DEFAULT_J2K_QUALITY.load(Ordering::Relaxed))
}

/// `sDefaultZstdLevel` (src/lib/OpenEXRCore/base.c:236-237 @ v3.5.3).
static S_DEFAULT_ZSTD_LEVEL: AtomicI32 = AtomicI32::new(5);

/// Port of `exr_set_default_zstd_compression_level` (src/lib/OpenEXRCore/base.c:239-245 @ v3.5.3).
pub fn set_default_zstd_compression_level(mut q: i32) {
    if q < 1 {
        q = 1;
    }
    if q > 22 {
        q = 22;
    }
    S_DEFAULT_ZSTD_LEVEL.store(q, Ordering::Relaxed);
}

/// Port of `exr_get_default_zstd_compression_level` (src/lib/OpenEXRCore/base.c:247-251 @ v3.5.3).
pub fn get_default_zstd_compression_level() -> i32 {
    S_DEFAULT_ZSTD_LEVEL.load(Ordering::Relaxed)
}
