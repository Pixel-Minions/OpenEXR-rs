// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `compression.c` (src/lib/OpenEXRCore/compression.c:1-760 @ v3.5.3), so far the
//! functions header parsing needs; the codec dispatch comes with Phase 4.

use crate::openexr_attr::Compression;

/// Port of `exr_compression_lines_per_chunk` (src/lib/OpenEXRCore/compression.c:321-348
/// @ v3.5.3): -1 for an invalid compression.
pub fn compression_lines_per_chunk(comptype: Compression) -> i32 {
    match comptype {
        Compression::NONE | Compression::RLE | Compression::ZSTD | Compression::ZIPS => 1,
        Compression::ZIP | Compression::PXR24 => 16,
        Compression::PIZ
        | Compression::B44
        | Compression::B44A
        | Compression::HTJ2K32
        | Compression::DWAA => 32,
        Compression::DWAB | Compression::HTJ2K256 | Compression::LJ2K => 256,
        // ERROR CONDITION
        _ => -1,
    }
}

/// Port of `exr_compression_is_valid_for_deep` (src/lib/OpenEXRCore/compression.c:500-514
/// @ v3.5.3).
pub fn compression_is_valid_for_deep(comptype: Compression) -> bool {
    matches!(
        comptype,
        Compression::NONE | Compression::RLE | Compression::ZIPS | Compression::ZSTD
    )
}

/// Port of `exr_compression_name` (src/lib/OpenEXRCore/compression.c:352-374 @ v3.5.3).
pub fn compression_name(comptype: Compression) -> &'static str {
    const COMPRESSIONNAMES: [&str; 14] = [
        "none", "rle", "zips", "zip", "piz", "pxr24", "b44", "b44a", "dwaa", "dwab", "htj2k256",
        "htj2k32", "lj2k", "zstd",
    ];
    let idx = comptype.0;
    if idx >= 0 && idx < Compression::LAST_TYPE.0 {
        return COMPRESSIONNAMES[idx as usize];
    }
    "<UNKNOWN>"
}
