// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_coding.h` (src/lib/OpenEXRCore/internal_coding.h:1-310 @ v3.5.3).
//!
//! So far the pixel type conversions (lines 116-304); the pipeline declarations come with
//! `coding.c` (Phase 4).

// Ported ahead of its callers (unpacking and packing: Phases 4 and 6).
#![allow(dead_code)]

use crate::imath_half::{imath_float_to_half, imath_half_to_float};

/// Port of `half_to_float` (src/lib/OpenEXRCore/internal_coding.h:118-168 @ v3.5.3): Imath
/// 3.2.3 is "safe for C", so this is `imath_half_to_float`.
#[inline]
pub(crate) fn half_to_float(hv: u16) -> f32 {
    imath_half_to_float(hv)
}

/// Port of `half_to_float_int` (src/lib/OpenEXRCore/internal_coding.h:170-180 @ v3.5.3).
#[inline]
pub(crate) fn half_to_float_int(hv: u16) -> u32 {
    half_to_float(hv).to_bits()
}

/// Port of `float_to_half` (src/lib/OpenEXRCore/internal_coding.h:182-229 @ v3.5.3): with
/// Imath 3.2.3, `imath_float_to_half`.
#[inline]
pub(crate) fn float_to_half(fv: f32) -> u16 {
    imath_float_to_half(fv)
}

/// Port of `float_to_half_int` (src/lib/OpenEXRCore/internal_coding.h:231-241 @ v3.5.3).
#[inline]
pub(crate) fn float_to_half_int(fiv: u32) -> u16 {
    float_to_half(f32::from_bits(fiv))
}

/// Port of `half_to_uint` (src/lib/OpenEXRCore/internal_coding.h:245-258 @ v3.5.3). A finite,
/// non-negative half is at most 65504, so the conversion to `uint32_t` is exact.
#[inline]
pub(crate) fn half_to_uint(hv: u16) -> u32 {
    if hv & 0x8000 != 0 {
        return 0;
    }
    if (hv & 0x7c00) == 0x7c00 {
        if (hv & 0x3ff) != 0 {
            return 0;
        }
        return u32::MAX;
    }
    half_to_float(hv) as u32
}

/// Port of `float_to_uint` (src/lib/OpenEXRCore/internal_coding.h:260-266 @ v3.5.3).
///
/// `fv > (float) UINT32_MAX` compares with 2^32, so exactly 2^32 reaches the conversion to
/// `uint32_t`, which is undefined in C. Both wheels' compilers convert through a 64-bit integer
/// and keep the low 32 bits, giving 0 (docs/improvements.md, I-2); `as i64` then `as u32` does
/// the same, and for every other value reaching it the conversion is exact truncation.
#[inline]
pub(crate) fn float_to_uint(fv: f32) -> u32 {
    if fv < 0.0 || fv.is_nan() {
        return 0;
    }
    if fv.is_infinite() || fv > u32::MAX as f32 {
        return u32::MAX;
    }
    fv as i64 as u32
}

/// Port of `float_to_uint_int` (src/lib/OpenEXRCore/internal_coding.h:268-278 @ v3.5.3).
#[inline]
pub(crate) fn float_to_uint_int(fint: u32) -> u32 {
    float_to_uint(f32::from_bits(fint))
}

/// Port of `uint_to_half` (src/lib/OpenEXRCore/internal_coding.h:280-286 @ v3.5.3).
#[inline]
pub(crate) fn uint_to_half(ui: u32) -> u16 {
    if ui > 65504 {
        return 0x7c00;
    }
    float_to_half(ui as f32)
}

/// Port of `uint_to_float` (src/lib/OpenEXRCore/internal_coding.h:288-292 @ v3.5.3): C's
/// conversion rounds to nearest, as Rust's `as` does.
#[inline]
pub(crate) fn uint_to_float(ui: u32) -> f32 {
    ui as f32
}

/// Port of `uint_to_float_int` (src/lib/OpenEXRCore/internal_coding.h:294-304 @ v3.5.3).
#[inline]
pub(crate) fn uint_to_float_int(ui: u32) -> u32 {
    uint_to_float(ui).to_bits()
}
