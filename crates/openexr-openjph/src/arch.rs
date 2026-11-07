// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_arch.h` (external/OpenJPH/src/core/openjph/ojph_arch.h:1-498 @ OpenJPH 0.32.0)
//! and `ojph_defs.h` (external/OpenJPH/src/core/openjph/ojph_defs.h:1-83 @ OpenJPH 0.32.0):
//! the bit-counting helpers, rounding and alignment. The CPU detection follows with the SIMD
//! kernels.

/// `NUM_FRAC_BITS` (external/OpenJPH/src/core/openjph/ojph_defs.h:67 @ OpenJPH 0.32.0).
pub(crate) const NUM_FRAC_BITS: i32 = 13;

/// `ojph_div_ceil` (external/OpenJPH/src/core/openjph/ojph_defs.h:70 @ OpenJPH 0.32.0), on
/// `ui32` (whose sum wraps, as upstream's does).
#[inline]
pub(crate) fn div_ceil(a: u32, b: u32) -> u32 {
    a.wrapping_add(b).wrapping_sub(1) / b
}

/// Port of `population_count` (external/OpenJPH/src/core/openjph/ojph_arch.h:186-201
/// @ OpenJPH 0.32.0).
#[inline]
pub(crate) fn population_count(val: u32) -> u32 {
    val.count_ones()
}

/// Port of `count_leading_zeros (ui32)` (external/OpenJPH/src/core/openjph/ojph_arch.h:207-223
/// @ OpenJPH 0.32.0). Of 0, MSVC's `_BitScanReverse` leaves its result 0, so the Windows
/// build answers 31; GCC's `__builtin_clz (0)` is undefined, and this answers 32 there
/// (`lzcnt`'s answer). Callers never pass 0 where it changes an output.
#[inline]
pub(crate) fn count_leading_zeros(val: u32) -> u32 {
    if val == 0 && cfg!(target_env = "msvc") {
        return 31;
    }
    val.leading_zeros()
}

/// Port of `count_leading_zeros (ui64)` (external/OpenJPH/src/core/openjph/ojph_arch.h:235-264
/// @ OpenJPH 0.32.0); of 0, 63 on Windows, as [`count_leading_zeros`].
#[inline]
pub(crate) fn count_leading_zeros64(val: u64) -> u32 {
    if val == 0 && cfg!(target_env = "msvc") {
        return 63;
    }
    val.leading_zeros()
}

/// Port of `count_trailing_zeros (ui32)` (external/OpenJPH/src/core/openjph/ojph_arch.h:270-286
/// @ OpenJPH 0.32.0); never of 0.
#[inline]
pub(crate) fn count_trailing_zeros(val: u32) -> u32 {
    val.trailing_zeros()
}

/// Port of `count_trailing_zeros (ui64)` (external/OpenJPH/src/core/openjph/ojph_arch.h:292-315
/// @ OpenJPH 0.32.0); never of 0.
#[inline]
pub(crate) fn count_trailing_zeros64(val: u64) -> u32 {
    val.trailing_zeros()
}

/// `(si32) f` as x86-64's `cvttss2si` computes it: out of range (or NaN), `0x80000000`.
#[inline]
pub(crate) fn cvtt_f32_i32(f: f32) -> i32 {
    if f >= -2147483648.0 && f < 2147483648.0 {
        f as i32
    } else {
        i32::MIN
    }
}

/// Port of `ojph_round` (external/OpenJPH/src/core/openjph/ojph_arch.h:318-327
/// @ OpenJPH 0.32.0): `(si32) (val + (val >= 0 ? 0.5f : -0.5f))`, the sum in `float`.
#[inline]
pub(crate) fn ojph_round(val: f32) -> i32 {
    cvtt_f32_i32(val + if val >= 0.0 { 0.5 } else { -0.5 })
}

/// Port of `ojph_trunc` (external/OpenJPH/src/core/openjph/ojph_arch.h:330-339
/// @ OpenJPH 0.32.0).
#[inline]
pub(crate) fn ojph_trunc(val: f32) -> i32 {
    cvtt_f32_i32(val)
}

/// `byte_alignment` (external/OpenJPH/src/core/openjph/ojph_arch.h:345 @ OpenJPH 0.32.0): 64
/// bytes, which line buffers are padded to.
pub(crate) const BYTE_ALIGNMENT: u32 = 64;

/// Port of `calc_aligned_size` (external/OpenJPH/src/core/openjph/ojph_arch.h:360-366
/// @ OpenJPH 0.32.0): `size` elements of `elem` bytes, rounded up to `n` bytes, in elements.
#[inline]
pub(crate) fn calc_aligned_size(size: usize, elem: usize, n: usize) -> usize {
    let s = (size * elem + n - 1) & !(n - 1);
    s / elem
}
