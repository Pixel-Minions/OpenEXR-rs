// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_util.h` (src/lib/OpenEXRCore/internal_util.h:1-87 @ v3.5.3).

// Ported ahead of some of its callers (the coding pipeline: Phase 4).
#![allow(dead_code)]

/// Port of `compute_sampled_height` (src/lib/OpenEXRCore/internal_util.h:12-48 @ v3.5.3): the
/// number of rows of `height` rows starting at `start_y` that a channel sampled every
/// `y_sampling` rows has. C's `%` truncates toward zero, as Rust's does; `-start_y` and
/// `height - off` wrap as the wheels' machine code does (they are undefined in C for
/// `INT_MIN` and extreme heights).
pub(crate) fn compute_sampled_height(height: i32, y_sampling: i32, start_y: i32) -> i32 {
    if y_sampling <= 1 {
        return height;
    }
    if height == 1 {
        return if start_y % y_sampling == 0 { 1 } else { 0 };
    }
    // computed the number of times y % ysampling == 0, by computing interval based on first
    // and last time that occurs on the given range
    let off = if start_y < 0 {
        start_y.wrapping_neg() % y_sampling
    } else {
        let off = start_y % y_sampling;
        if off != 0 { y_sampling - off } else { off }
    };
    let mut tmph = height.wrapping_sub(off);
    // tmph is negative when the chunk holds none of the sampled rows
    if tmph <= 0 {
        return 0;
    }
    tmph -= 1;
    tmph / y_sampling + 1
}

/// Port of `compute_sampled_width` (src/lib/OpenEXRCore/internal_util.h:50-63 @ v3.5.3).
pub(crate) fn compute_sampled_width(width: i32, x_sampling: i32, _start_x: i32) -> i32 {
    if x_sampling <= 1 {
        return width;
    }
    if width == 1 { 1 } else { width / x_sampling }
}

/// Port of `checked_mul_accum_u64` (src/lib/OpenEXRCore/internal_util.h:72-85 @ v3.5.3):
/// `*accum += a * b`, or `true` (and `accum` unchanged) if either step would overflow.
pub(crate) fn checked_mul_accum_u64(accum: &mut u64, a: u64, b: u64) -> bool {
    if a != 0 && b > u64::MAX / a {
        return true;
    }
    let prod = a * b;
    let sum = accum.wrapping_add(prod);
    if sum < *accum {
        return true;
    }
    *accum = sum;
    false
}
