// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_codestream_gen.cpp`
//! (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:1-185 @ OpenJPH 0.32.0): the
//! generic conversions between a line's samples and a codeblock's sign and magnitude.

use crate::arch::ojph_trunc;

/// Port of `gen_find_max_val32` (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:53
/// @ OpenJPH 0.32.0).
pub(crate) fn find_max_val32(addr: &[u32]) -> u32 {
    addr[0]
}

/// Port of `gen_find_max_val64` (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:56
/// @ OpenJPH 0.32.0).
pub(crate) fn find_max_val64(addr: &[u64]) -> u64 {
    addr[0]
}

/// Port of `gen_rev_tx_to_cb32`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:59-78 @ OpenJPH 0.32.0).
pub(crate) fn rev_tx_to_cb32(
    sp: &[i32],
    dp: &mut [u32],
    k_max: u32,
    count: u32,
    max_val: &mut u32,
) {
    let shift = 31 - k_max;
    // convert to sign and magnitude and keep max_val
    let mut tmax = *max_val;
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let sign = if v >= 0 { 0u32 } else { 0x8000_0000 };
        let mut val = (if v >= 0 { v } else { v.wrapping_neg() }) as u32;
        val <<= shift;
        *d = sign | val;
        tmax |= val; // it is more efficient to use or than max
    }
    *max_val = tmax;
}

/// Port of `gen_rev_tx_to_cb64`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:81-100 @ OpenJPH 0.32.0).
pub(crate) fn rev_tx_to_cb64(
    sp: &[i64],
    dp: &mut [u64],
    k_max: u32,
    count: u32,
    max_val: &mut u64,
) {
    let shift = 63 - k_max;
    let mut tmax = *max_val;
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let sign = if v >= 0 { 0u64 } else { 0x8000_0000_0000_0000 };
        let mut val = (if v >= 0 { v } else { v.wrapping_neg() }) as u64;
        val <<= shift;
        *d = sign | val;
        tmax |= val;
    }
    *max_val = tmax;
}

/// Port of `gen_irv_tx_to_cb32`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:103-121 @ OpenJPH 0.32.0): quantized by `delta_inv`.
pub(crate) fn irv_tx_to_cb32(
    sp: &[f32],
    dp: &mut [u32],
    delta_inv: f32,
    count: u32,
    max_val: &mut u32,
) {
    // quantize and convert to sign and magnitude and keep max_val
    let mut tmax = *max_val;
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let t = ojph_trunc(v * delta_inv);
        let sign = if t >= 0 { 0u32 } else { 0x8000_0000 };
        let val = (if t >= 0 { t } else { t.wrapping_neg() }) as u32;
        *d = sign | val;
        tmax |= val;
    }
    *max_val = tmax;
}

/// Port of `gen_rev_tx_from_cb32`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:124-137 @ OpenJPH 0.32.0).
pub(crate) fn rev_tx_from_cb32(sp: &[u32], dp: &mut [i32], k_max: u32, count: u32) {
    let shift = 31 - k_max;
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let val = ((v & 0x7FFF_FFFF) >> shift) as i32;
        *d = if (v & 0x8000_0000) != 0 { -val } else { val };
    }
}

/// Port of `gen_rev_tx_from_cb64`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:140-153 @ OpenJPH 0.32.0).
pub(crate) fn rev_tx_from_cb64(sp: &[u64], dp: &mut [i64], k_max: u32, count: u32) {
    let shift = 63 - k_max;
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let val = ((v & 0x7FFF_FFFF_FFFF_FFFF) >> shift) as i64;
        *d = if (v & 0x8000_0000_0000_0000) != 0 {
            -val
        } else {
            val
        };
    }
}

/// Port of `gen_irv_tx_from_cb32`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:156-168 @ OpenJPH 0.32.0): dequantized by `delta`.
pub(crate) fn irv_tx_from_cb32(sp: &[u32], dp: &mut [f32], delta: f32, count: u32) {
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let val = (v & 0x7FFF_FFFF) as f32 * delta;
        *d = if (v & 0x8000_0000) != 0 { -val } else { val };
    }
}

/// Port of `gen_irv_tx_from_cb64`
/// (external/OpenJPH/src/core/codestream/ojph_codestream_gen.cpp:171-183 @ OpenJPH 0.32.0).
pub(crate) fn irv_tx_from_cb64(sp: &[u64], dp: &mut [f32], delta: f32, count: u32) {
    for (d, &v) in dp[..count as usize].iter_mut().zip(sp) {
        let val = (v & 0x7FFF_FFFF_FFFF_FFFF) as f32 * delta;
        *d = if (v & 0x8000_0000_0000_0000) != 0 {
            -val
        } else {
            val
        };
    }
}
