// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_helpers.h` (src/lib/OpenEXRCore/internal_dwa_helpers.h:1-69 @ v3.5.3):
//! DWA's shared enums and the layout of a chunk's size counters.

/// `AcCompression`: how the AC coefficients are compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcCompression {
    StaticHuffman = 0,
    Deflate = 1,
}

/// `CompressorScheme`: how a channel is compressed. `NUM_COMPRESSOR_SCHEMES` is 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompressorScheme {
    Unknown = 0,
    LossyDct = 1,
    Rle = 2,
}

/// `NUM_COMPRESSOR_SCHEMES`.
pub(crate) const NUM_COMPRESSOR_SCHEMES: usize = 3;

impl CompressorScheme {
    /// The scheme of a stored value (0 to 2).
    pub(crate) fn from_index(v: u8) -> Option<Self> {
        match v {
            0 => Some(CompressorScheme::Unknown),
            1 => Some(CompressorScheme::LossyDct),
            2 => Some(CompressorScheme::Rle),
            _ => None,
        }
    }
}

// `DataSizesSingle`: the per-chunk counters at the head of a chunk, in this order.
/// Version number: 0 classic, 1 adds "end of block" to the AC RLE, 2 adds the channel rules.
pub(crate) const VERSION: usize = 0;
/// Size of leftover data, uncompressed.
pub(crate) const UNKNOWN_UNCOMPRESSED_SIZE: usize = 1;
/// Size of leftover data, zlib compressed.
pub(crate) const UNKNOWN_COMPRESSED_SIZE: usize = 2;
/// AC RLE + Huffman size.
pub(crate) const AC_COMPRESSED_SIZE: usize = 3;
/// DC + Deflate size.
pub(crate) const DC_COMPRESSED_SIZE: usize = 4;
/// RLE + Deflate data size.
pub(crate) const RLE_COMPRESSED_SIZE: usize = 5;
/// RLE'd data size.
pub(crate) const RLE_UNCOMPRESSED_SIZE: usize = 6;
/// Un-RLE'd data size.
pub(crate) const RLE_RAW_SIZE: usize = 7;
/// AC RLE number of elements.
pub(crate) const AC_UNCOMPRESSED_COUNT: usize = 8;
/// DC number of elements.
pub(crate) const DC_UNCOMPRESSED_COUNT: usize = 9;
/// AC compression strategy.
pub(crate) const AC_COMPRESSION: usize = 10;
/// `NUM_SIZES_SINGLE`.
pub(crate) const NUM_SIZES_SINGLE: usize = 11;

/// Port of `std_max` (src/lib/OpenEXRCore/internal_dwa_helpers.h:55-59 @ v3.5.3).
pub(crate) fn std_max(a: u64, b: u64) -> u64 {
    if a < b { b } else { a }
}
