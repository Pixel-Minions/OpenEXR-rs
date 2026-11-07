// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of the parts of `mem.h` (external/zstd/lib/common/mem.h:1-422 @ zstd v1.5.7) and
//! `bits.h` (external/zstd/lib/common/bits.h:1-200 @ zstd v1.5.7) the port uses: little-endian
//! reads and writes, and bit scans.

/// `MEM_readLE16`.
#[inline]
pub(crate) fn read_le16(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}

/// `MEM_readLE24`.
#[inline]
pub(crate) fn read_le24(p: &[u8]) -> u32 {
    u32::from(read_le16(p)) + (u32::from(p[2]) << 16)
}

/// `MEM_readLE32`.
#[inline]
pub(crate) fn read_le32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}

/// `MEM_readLE64` (and `MEM_readLEST` on 64-bit hosts).
#[inline]
pub(crate) fn read_le64(p: &[u8]) -> u64 {
    u64::from_le_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]])
}

/// Port of `ZSTD_highbit32` (external/zstd/lib/common/bits.h:174-178 @ zstd v1.5.7): the index
/// of the highest set bit; `val` is not 0.
#[inline]
pub(crate) fn highbit32(val: u32) -> u32 {
    31 - val.leading_zeros()
}

/// Port of `ZSTD_countTrailingZeros32` (external/zstd/lib/common/bits.h:28-50
/// @ zstd v1.5.7); `val` is not 0.
#[inline]
pub(crate) fn count_trailing_zeros32(val: u32) -> u32 {
    val.trailing_zeros()
}
