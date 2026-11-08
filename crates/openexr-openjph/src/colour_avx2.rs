// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_colour_avx2.cpp`
//! (external/OpenJPH/src/core/transform/ojph_colour_avx2.cpp:1-823 @ OpenJPH 0.32.0), the
//! kernels `init_colour_transform_functions` chooses at `X86_CPU_EXT_LEVEL_AVX2`. The LUT
//! nonlinearity conversions (`local_avx2_irv_convert_to_integer_nlt2or4` and
//! `local_avx2_irv_convert_to_float_nlt2or4`, lines 353-582) in AVX2: eight samples a step,
//! the table read by gathers. Each lane computes as the port's per-sample loop (`colour.rs`),
//! which finishes a line's last samples; upstream's lanes past the width write only padding.
//! The other conversions and the reversible colour transform as AVX2 builds of the generic
//! loops (below).

#![allow(unsafe_code)]

use std::arch::x86_64::*;

use crate::mem::LineBuf;
use crate::params::NltRec;

/// The largest table index a clamped sample reaches, `(ui32) ((d_max - d_min) * inv_delta)`
/// (the index grows with the sample, which is clamped to `d_max`), with the table's length
/// checked to hold it and the next entry: the gathers' bound, or `None`.
fn gather_bound(lut: &[f32], d_min: f32, d_max: f32, inv_delta: f32) -> Option<()> {
    let k = (d_max - d_min) * inv_delta;
    if !(k >= 0.0 && k < 2147483648.0) {
        return None;
    }
    // the gathers read lut[k + 1] and lut[k + 2] (dec_points / enc_points from lut[1])
    ((k as usize) + 2 < lut.len()).then_some(())
}

/// Port of the loops of `local_avx2_irv_convert_to_integer_nlt2or4` (lines 353-436): the
/// first `8 * (width / 8)` samples; returns how many were done (0 when the table's bound
/// does not hold, leaving the line to the per-sample loop).
///
/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
unsafe fn to_integer_nlt2or4(
    nlt4: bool,
    sp: &[f32],
    dp: &mut [i32],
    width: usize,
    rec: &NltRec,
) -> usize {
    let lut = &rec.dec;
    if gather_bound(lut, rec.fd_min, rec.fd_max, rec.inv_delta).is_none() {
        return 0;
    }
    let n = (width / 8) * 8;
    let (sp, dp) = (&sp[..n], &mut dp[..n]);
    let mul = _mm256_set1_ps(rec.multiplier);
    let d_min = _mm256_set1_ps(rec.fd_min);
    let d_max = _mm256_set1_ps(rec.fd_max);
    let delta = _mm256_set1_ps(rec.delta);
    let inv_delta = _mm256_set1_ps(rec.inv_delta);
    // dec_points is lut[1..]
    let points = lut[1..].as_ptr();
    let half_ps = _mm256_set1_ps(0.5);
    let one = _mm256_set1_epi32(1);

    let signed = rec.is_signed();
    let half = _mm256_set1_ps((1u64 << (rec.get_bit_depth().max(1) - 1)) as f32);
    let bias = _mm256_set1_epi32(-(((1u64 << (rec.get_bit_depth().max(1) - 1)) + 1) as i32));
    let zero = _mm256_setzero_si256();

    let mut i = 0;
    while i < n {
        // SAFETY: i + 8 <= n, inside both lines; the gathers' indices are at most the
        // table's bound (gather_bound)
        unsafe {
            let mut t = _mm256_loadu_ps(sp.as_ptr().add(i));
            t = _mm256_add_ps(t, half_ps); // convert to [0, 1]
            t = _mm256_max_ps(t, d_min);
            t = _mm256_min_ps(t, d_max);
            let k = _mm256_cvttps_epi32(_mm256_mul_ps(_mm256_sub_ps(t, d_min), inv_delta));
            let d_k = _mm256_add_ps(d_min, _mm256_mul_ps(_mm256_cvtepi32_ps(k), delta));
            let t_k = _mm256_i32gather_ps::<4>(points, k);
            let t_kp1 = _mm256_i32gather_ps::<4>(points, _mm256_add_epi32(k, one));
            let z = _mm256_add_ps(
                t_k,
                _mm256_mul_ps(
                    _mm256_mul_ps(_mm256_sub_ps(t, d_k), inv_delta),
                    _mm256_sub_ps(t_kp1, t_k),
                ),
            );
            let v = if signed {
                let mut v = _mm256_cvtps_epi32(_mm256_sub_ps(_mm256_mul_ps(z, mul), half));
                if nlt4 {
                    let c = _mm256_cmpgt_epi32(zero, v); // 0xFFFFFFFF for -ve val
                    let mut neg = _mm256_sub_epi32(bias, v); // - bias - value
                    neg = _mm256_and_si256(c, neg); // keep only - bias - val
                    v = _mm256_andnot_si256(c, v); // keep only +ve or 0
                    v = _mm256_or_si256(neg, v); // combine
                }
                v
            } else {
                _mm256_cvtps_epi32(_mm256_mul_ps(z, mul))
            };
            _mm256_storeu_si256(dp.as_mut_ptr().add(i) as *mut __m256i, v);
        }
        i += 8;
    }
    n
}

/// Port of the loops of `local_avx2_irv_convert_to_float_nlt2or4` (lines 497-582): the first
/// `8 * (width / 8)` samples; returns how many were done (as [`to_integer_nlt2or4`]).
///
/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
unsafe fn to_float_nlt2or4(
    nlt4: bool,
    sp: &[i32],
    dp: &mut [f32],
    bit_depth: u32,
    width: usize,
    rec: &NltRec,
) -> usize {
    let lut = &rec.enc;
    if gather_bound(lut, rec.ft_min, rec.ft_max, rec.inv_delta).is_none() {
        return 0;
    }
    let n = (width / 8) * 8;
    let (sp, dp) = (&sp[..n], &mut dp[..n]);
    let mul = _mm256_set1_ps((1.0 / (1u64 << bit_depth) as f64) as f32);
    let d_min = _mm256_set1_ps(rec.ft_min);
    let d_max = _mm256_set1_ps(rec.ft_max);
    let delta = _mm256_set1_ps(rec.delta);
    let inv_delta = _mm256_set1_ps(rec.inv_delta);
    // enc_points is lut[1..]
    let points = lut[1..].as_ptr();
    let half_ps = _mm256_set1_ps(0.5);
    let one = _mm256_set1_epi32(1);

    let signed = rec.is_signed();
    let bias = _mm256_set1_epi32(-(((1u64 << (rec.get_bit_depth().max(1) - 1)) + 1) as i32));
    let zero = _mm256_setzero_si256();

    let mut i = 0;
    while i < n {
        // SAFETY: as to_integer_nlt2or4's
        unsafe {
            let mut v = _mm256_loadu_si256(sp.as_ptr().add(i) as *const __m256i);
            let mut t = if signed {
                if nlt4 {
                    let c = _mm256_cmpgt_epi32(zero, v); // 0xFFFFFFFF for -ve val
                    let mut neg = _mm256_sub_epi32(bias, v); // - bias - value
                    neg = _mm256_and_si256(c, neg); // keep only - bias - val
                    v = _mm256_andnot_si256(c, v); // keep only +ve or 0
                    v = _mm256_or_si256(neg, v); // combine
                }
                _mm256_add_ps(_mm256_mul_ps(_mm256_cvtepi32_ps(v), mul), half_ps) // to [0, 1]
            } else {
                _mm256_mul_ps(_mm256_cvtepi32_ps(v), mul) // it is in [0, 1]
            };
            t = _mm256_max_ps(t, d_min);
            t = _mm256_min_ps(t, d_max);
            let k = _mm256_cvttps_epi32(_mm256_mul_ps(_mm256_sub_ps(t, d_min), inv_delta));
            let d_k = _mm256_add_ps(d_min, _mm256_mul_ps(_mm256_cvtepi32_ps(k), delta));
            let t_k = _mm256_i32gather_ps::<4>(points, k);
            let t_kp1 = _mm256_i32gather_ps::<4>(points, _mm256_add_epi32(k, one));
            let y = _mm256_add_ps(
                t_k,
                _mm256_mul_ps(
                    _mm256_mul_ps(_mm256_sub_ps(t, d_k), inv_delta),
                    _mm256_sub_ps(t_kp1, t_k),
                ),
            );
            _mm256_storeu_ps(dp.as_mut_ptr().add(i), _mm256_sub_ps(y, half_ps));
        }
        i += 8;
    }
    n
}

/// [`to_integer_nlt2or4`] where the CPU has AVX2; the samples done (0 without AVX2).
pub(crate) fn irv_to_integer_nlt2or4(
    nlt4: bool,
    sp: &[f32],
    dp: &mut [i32],
    width: usize,
    rec: &NltRec,
) -> usize {
    if !std::arch::is_x86_feature_detected!("avx2") || sp.len() < width || dp.len() < width {
        return 0;
    }
    // SAFETY: the CPU has AVX2
    unsafe { to_integer_nlt2or4(nlt4, sp, dp, width, rec) }
}

/// [`to_float_nlt2or4`] where the CPU has AVX2; the samples done (0 without AVX2).
pub(crate) fn irv_to_float_nlt2or4(
    nlt4: bool,
    sp: &[i32],
    dp: &mut [f32],
    bit_depth: u32,
    width: usize,
    rec: &NltRec,
) -> usize {
    if !std::arch::is_x86_feature_detected!("avx2") || sp.len() < width || dp.len() < width {
        return 0;
    }
    // SAFETY: the CPU has AVX2
    unsafe { to_float_nlt2or4(nlt4, sp, dp, bit_depth, width, rec) }
}

/// The CPU has AVX2 (upstream's level check implies it).
fn avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

// The conversions and the reversible colour transform `init_colour_transform_functions`
// chooses at `X86_CPU_EXT_LEVEL_AVX2` (`avx2_rev_convert`, `avx2_rev_convert_nlt_type3`,
// `avx2_irv_convert_to_integer`, `avx2_irv_convert_to_float` and their type-3 variants,
// `avx2_rct_forward`, `avx2_rct_backward`, external/OpenJPH/src/core/transform/
// ojph_colour_avx2.cpp:70-820 @ OpenJPH 0.32.0): the generic loops (`colour.rs`), whose
// samples upstream's lanes compute (wrapping integer adds, the 64-bit `srai` emulation exact
// for its shift of 2, `cvtepi32_ps`, and `cvtps2dq` with AVX2's clamps), compiled for AVX2.
macro_rules! avx2_build {
    ($wrapper:ident, $inner:ident, $k:path, ($($p:ident: $t:ty),*)) => {
        /// # Safety
        /// The CPU supports AVX2.
        #[target_feature(enable = "avx2")]
        unsafe fn $inner($($p: $t),*) {
            $k($($p),*);
        }

        /// The kernel's AVX2 build; false where the CPU lacks AVX2.
        pub(crate) fn $wrapper($($p: $t),*) -> bool {
            if !avx2() {
                return false;
            }
            // SAFETY: the CPU has AVX2
            unsafe { $inner($($p),*) };
            true
        }
    };
}

avx2_build!(
    rev_convert,
    rev_convert_avx2,
    crate::colour::rev_convert_gen,
    (src_line: &LineBuf, src_line_offset: u32, dst_line: &mut LineBuf, dst_line_offset: u32, shift: i64, width: u32)
);

avx2_build!(
    rev_convert_nlt_type3,
    rev_convert_nlt_type3_avx2,
    crate::colour::rev_convert_nlt_type3_gen,
    (src_line: &LineBuf, src_line_offset: u32, dst_line: &mut LineBuf, dst_line_offset: u32, shift: i64, width: u32)
);

avx2_build!(
    irv_convert_to_integer,
    irv_convert_to_integer_avx2,
    crate::colour::irv_convert_to_integer_gen,
    (nlt_type3: bool, src_line: &LineBuf, dst_line: &mut LineBuf, dst_line_offset: u32, bit_depth: u32, is_signed: bool, width: u32)
);

avx2_build!(
    irv_convert_to_float,
    irv_convert_to_float_avx2,
    crate::colour::irv_convert_to_float_gen,
    (nlt_type3: bool, src_line: &LineBuf, src_line_offset: u32, dst_line: &mut LineBuf, bit_depth: u32, is_signed: bool, width: u32)
);

avx2_build!(
    rct_forward,
    rct_forward_avx2,
    crate::colour::rct_forward_gen,
    (r: &LineBuf, g: &LineBuf, b: &LineBuf, y: &mut LineBuf, cb: &mut LineBuf, cr: &mut LineBuf, repeat: u32)
);

avx2_build!(
    rct_backward,
    rct_backward_avx2,
    crate::colour::rct_backward_gen,
    (y: &LineBuf, cb: &LineBuf, cr: &LineBuf, r: &mut LineBuf, g: &mut LineBuf, b: &mut LineBuf, repeat: u32)
);
