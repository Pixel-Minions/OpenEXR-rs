// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_zstd_delta.c` (src/lib/OpenEXRCore/internal_zstd_delta.c:1-276
//! @ v3.5.3): the delta along a row's samples, on little-endian 16- and 32-bit values. GCC
//! builds decode with AVX2 where the CPU has it, which computes the same prefix sums; the
//! scalar loops are ported.

use crate::internal_xdr::{
    unaligned_load16, unaligned_load32, unaligned_store16, unaligned_store32,
};

/// Port of `delta_encode_row_u16_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:16-26
/// @ v3.5.3): each value minus the one before it, as the original values were. Upstream walks
/// the row backwards so that each value's predecessor is still the original; walking it
/// forwards with the predecessor kept aside gives the same differences.
pub(crate) fn delta_encode_row_u16(p: &mut [u8], n: u64) {
    if n <= 1 {
        return;
    }
    let row = &mut p[..n as usize * 2];
    let mut prev = unaligned_load16(row);
    for c in row[2..].as_chunks_mut::<2>().0 {
        let a = unaligned_load16(c);
        unaligned_store16(c, a.wrapping_sub(prev));
        prev = a;
    }
}

/// Port of `delta_decode_row_u16_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:28-37
/// @ v3.5.3): the running sum, each value plus the (decoded) one before it.
pub(crate) fn delta_decode_row_u16(p: &mut [u8], n: u64) {
    // the AVX2 kernel where the CPU has AVX2, as GCC's builds take it (`internal_zstd_simd`)
    #[cfg(target_arch = "x86_64")]
    if crate::internal_zstd_simd::delta_decode_row_u16(p, n) {
        return;
    }
    if n <= 1 {
        return;
    }
    let row = &mut p[..n as usize * 2];
    let mut prev = unaligned_load16(row);
    for c in row[2..].as_chunks_mut::<2>().0 {
        prev = unaligned_load16(c).wrapping_add(prev);
        unaligned_store16(c, prev);
    }
}

/// Port of `delta_encode_row_u32_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:39-49
/// @ v3.5.3), as [`delta_encode_row_u16`].
pub(crate) fn delta_encode_row_u32(p: &mut [u8], n: u64) {
    if n <= 1 {
        return;
    }
    let row = &mut p[..n as usize * 4];
    let mut prev = unaligned_load32(row);
    for c in row[4..].as_chunks_mut::<4>().0 {
        let a = unaligned_load32(c);
        unaligned_store32(c, a.wrapping_sub(prev));
        prev = a;
    }
}

/// Port of `delta_decode_row_u32_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:51-60
/// @ v3.5.3), as [`delta_decode_row_u16`].
pub(crate) fn delta_decode_row_u32(p: &mut [u8], n: u64) {
    #[cfg(target_arch = "x86_64")]
    if crate::internal_zstd_simd::delta_decode_row_u32(p, n) {
        return;
    }
    if n <= 1 {
        return;
    }
    let row = &mut p[..n as usize * 4];
    let mut prev = unaligned_load32(row);
    for c in row[4..].as_chunks_mut::<4>().0 {
        prev = unaligned_load32(c).wrapping_add(prev);
        unaligned_store32(c, prev);
    }
}
