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
/// @ v3.5.3).
pub(crate) fn delta_encode_row_u16(p: &mut [u8], n: u64) {
    if n <= 1 {
        return;
    }
    for k in (1..n as usize).rev() {
        let a = unaligned_load16(&p[k * 2..]);
        let b = unaligned_load16(&p[(k - 1) * 2..]);
        unaligned_store16(&mut p[k * 2..], a.wrapping_sub(b));
    }
}

/// Port of `delta_decode_row_u16_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:28-37
/// @ v3.5.3).
pub(crate) fn delta_decode_row_u16(p: &mut [u8], n: u64) {
    for k in 1..n as usize {
        let a = unaligned_load16(&p[k * 2..]);
        let b = unaligned_load16(&p[(k - 1) * 2..]);
        unaligned_store16(&mut p[k * 2..], a.wrapping_add(b));
    }
}

/// Port of `delta_encode_row_u32_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:39-49
/// @ v3.5.3).
pub(crate) fn delta_encode_row_u32(p: &mut [u8], n: u64) {
    if n <= 1 {
        return;
    }
    for k in (1..n as usize).rev() {
        let a = unaligned_load32(&p[k * 4..]);
        let b = unaligned_load32(&p[(k - 1) * 4..]);
        unaligned_store32(&mut p[k * 4..], a.wrapping_sub(b));
    }
}

/// Port of `delta_decode_row_u32_scalar` (src/lib/OpenEXRCore/internal_zstd_delta.c:51-60
/// @ v3.5.3).
pub(crate) fn delta_decode_row_u32(p: &mut [u8], n: u64) {
    for k in 1..n as usize {
        let a = unaligned_load32(&p[k * 4..]);
        let b = unaligned_load32(&p[(k - 1) * 4..]);
        unaligned_store32(&mut p[k * 4..], a.wrapping_add(b));
    }
}
