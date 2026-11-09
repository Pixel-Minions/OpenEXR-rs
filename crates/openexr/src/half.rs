// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Halves, as the API holds them: `u16` bit patterns, converted exactly as Core converts them
//! (Imath 3.2.3's `imath_half_to_float` and `imath_float_to_half`).

/// The `f32` a half's bits stand for (exact: every half is a float).
pub fn to_f32(h: u16) -> f32 {
    openexr_core::imath_half::imath_half_to_float(h)
}

/// The half nearest `f` (ties to even), as its bits.
pub fn from_f32(f: f32) -> u16 {
    openexr_core::imath_half::imath_float_to_half(f)
}
