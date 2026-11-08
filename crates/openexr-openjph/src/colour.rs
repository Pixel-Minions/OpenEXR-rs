// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_colour.cpp` (external/OpenJPH/src/core/transform/ojph_colour.cpp:1-818
//! @ OpenJPH 0.32.0) and `ojph_colour_local.h`
//! (external/OpenJPH/src/core/transform/ojph_colour_local.h:1-422 @ OpenJPH 0.32.0), its
//! generic functions: the sample conversions (with the nonlinearities), and the reversible
//! (RCT) and irreversible (ICT) colour transforms. On x86-64 upstream runs SSE2 or AVX2
//! kernels instead; the integer ones compute the same values, the float ones follow with the
//! SIMD kernels.

use crate::arch::{cvt_f32_i32, get_cpu_ext_level, ojph_round, x86_level};
use crate::mem::{LFT_32BIT, LineBuf};
use crate::params::{NltRec, OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT, OJPH_NLT_LUT_STYLE_NLT};

/// `ojph_max (a, b)`: `a > b ? a : b`.
#[inline]
fn fmax(a: f32, b: f32) -> f32 {
    if a > b { a } else { b }
}

/// `ojph_min (a, b)`: `a < b ? a : b`.
#[inline]
fn fmin(a: f32, b: f32) -> f32 {
    if a < b { a } else { b }
}

/// `(ui32) floorf (x)` of a value in `[0, 2^32)`.
#[inline]
fn floor_u32(x: f32) -> u32 {
    x.floor() as u32
}

/// The kernels `init_colour_transform_functions`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:132-254 @ OpenJPH 0.32.0) chooses for
/// the conversions of floats to integers: the generic ones, SSE2's
/// (external/OpenJPH/src/core/transform/ojph_colour_sse2.cpp:91-313 @ OpenJPH 0.32.0) or
/// AVX2's (external/OpenJPH/src/core/transform/ojph_colour_avx2.cpp:245-452 @ OpenJPH 0.32.0).
/// The SIMD ones round with `cvtps2dq` (to nearest even, `0x80000000` out of range) where the
/// generic ones use `ojph_round`, and AVX2's clamps let a NaN through (unordered compares)
/// where SSE2's and the generic ones clamp it to the upper limit. Each lane computes as the
/// port's sample; past the width upstream's lanes write into padding or into the next tile's
/// samples, which that tile writes after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToIntKernel {
    Generic,
    Sse2,
    Avx2,
}

fn to_int_kernel() -> ToIntKernel {
    let level = get_cpu_ext_level();
    if cfg!(target_arch = "x86_64") && level >= x86_level::AVX2 {
        ToIntKernel::Avx2
    } else if cfg!(target_arch = "x86_64") && level >= x86_level::SSE2 {
        ToIntKernel::Sse2
    } else {
        ToIntKernel::Generic
    }
}

impl ToIntKernel {
    /// `ojph_round`, or `_mm_cvtps_epi32`.
    #[inline]
    fn round(self, t: f32) -> i32 {
        match self {
            ToIntKernel::Generic => ojph_round(t),
            _ => cvt_f32_i32(t),
        }
    }

    /// `t >= low ? v : low_lim`, or AVX2's `ojph_mm256_max_ge_epi32` (`_CMP_NLT_UQ`).
    #[inline]
    fn max_ge(self, v: i32, low_lim: i32, t: f32, low: f32) -> i32 {
        let keep = match self {
            ToIntKernel::Avx2 => !(t < low),
            _ => t >= low,
        };
        if keep { v } else { low_lim }
    }

    /// `t < up ? v : up_lim`, or AVX2's `ojph_mm256_min_lt_epi32` (`_CMP_NGE_UQ`).
    #[inline]
    fn min_lt(self, v: i32, up_lim: i32, t: f32, up: f32) -> i32 {
        let keep = match self {
            ToIntKernel::Avx2 => !(t >= up),
            _ => t < up,
        };
        if keep { v } else { up_lim }
    }
}

/// Port of `local_gen_irv_convert_to_integer_nlt2or4`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:272-332 @ OpenJPH 0.32.0): floats
/// through the decoding table of a LUT nonlinearity (type 2 or 4) to integers.
fn irv_convert_to_integer_nlt2or4(
    nlt_type: u8,
    src_line: &LineBuf,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    width: u32,
    rec: &NltRec,
) {
    let sp = src_line.f32();
    let dp = &mut dst_line.i32_mut()[dst_line_offset as usize..];
    let mul = rec.multiplier;
    let d_min = rec.fd_min;
    let d_max = rec.fd_max;
    let delta = rec.delta;
    let inv_delta = rec.inv_delta;
    // dec_points[k] is lut[k + 1]
    let lut = &rec.dec;
    let kernel = to_int_kernel();
    // AVX2's kernel (colour_avx2.rs) does the samples of whole steps of 8, where upstream's
    // takes it; the loops below do the rest, each lane as they would
    #[cfg(target_arch = "x86_64")]
    let start = if kernel == ToIntKernel::Avx2 {
        crate::colour_avx2::irv_to_integer_nlt2or4(nlt_type == 4, sp, dp, width as usize, rec)
    } else {
        0
    };
    #[cfg(not(target_arch = "x86_64"))]
    let start = 0;
    if rec.is_signed() {
        let bias = ((1u64 << (rec.get_bit_depth() - 1)) + 1) as i32;
        let half = (1u64 << (rec.get_bit_depth() - 1)) as f32;
        for i in start..width as usize {
            let mut t = sp[i] + 0.5; // convert to [0, 1]
            t = fmax(t, d_min);
            t = fmin(t, d_max);
            let k = floor_u32((t - d_min) * inv_delta);
            let d_k = d_min + k as f32 * delta;
            let t_k = lut[k as usize + 1];
            let t_kp1 = lut[k as usize + 2];
            let z = t_k + (t - d_k) * inv_delta * (t_kp1 - t_k);
            let mut v = kernel.round(z * mul - half);
            if nlt_type == 4 {
                v = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(bias)
                };
            }
            dp[i] = v;
        }
    } else {
        for i in start..width as usize {
            let mut t = sp[i] + 0.5; // convert to [0, 1]
            t = fmax(t, d_min);
            t = fmin(t, d_max);
            let k = floor_u32((t - d_min) * inv_delta);
            let d_k = d_min + k as f32 * delta;
            let t_k = lut[k as usize + 1];
            let t_kp1 = lut[k as usize + 2];
            let z = t_k + (t - d_k) * inv_delta * (t_kp1 - t_k);
            dp[i] = kernel.round(z * mul);
        }
    }
}

/// Port of `gen_irv_convert_to_integer_nlt`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:335-355 @ OpenJPH 0.32.0).
pub(crate) fn irv_convert_to_integer_nlt(
    src_line: &LineBuf,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    width: u32,
    rec: &NltRec,
) {
    if rec.get_type() == OJPH_NLT_LUT_STYLE_NLT {
        irv_convert_to_integer_nlt2or4(2, src_line, dst_line, dst_line_offset, width, rec);
    } else if rec.get_type() == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT {
        irv_convert_to_integer_nlt2or4(4, src_line, dst_line, dst_line_offset, width, rec);
    }
}

/// Port of `local_gen_irv_convert_to_float_nlt2or4`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:358-413 @ OpenJPH 0.32.0): integers
/// through the encoding table of a LUT nonlinearity to floats.
#[allow(clippy::too_many_arguments)]
fn irv_convert_to_float_nlt2or4(
    nlt_type: u8,
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    bit_depth: u32,
    width: u32,
    rec: &NltRec,
) {
    let mul = (1.0 / (1u64 << bit_depth) as f64) as f32;
    let d_min = rec.ft_min;
    let d_max = rec.ft_max;
    let delta = rec.delta;
    let inv_delta = rec.inv_delta;
    // enc_points[k] is lut[k + 1]
    let lut = &rec.enc;
    let sp = &src_line.i32()[src_line_offset as usize..];
    let dp = dst_line.f32_mut();
    // AVX2's kernel (colour_avx2.rs) does the samples of whole steps of 8, where upstream's
    // takes it (from X86_CPU_EXT_LEVEL_AVX2); the loops below do the rest
    #[cfg(target_arch = "x86_64")]
    let start = if to_int_kernel() == ToIntKernel::Avx2 {
        crate::colour_avx2::irv_to_float_nlt2or4(
            nlt_type == 4,
            sp,
            dp,
            bit_depth,
            width as usize,
            rec,
        )
    } else {
        0
    };
    #[cfg(not(target_arch = "x86_64"))]
    let start = 0;
    if rec.is_signed() {
        let bias = ((1u64 << (rec.get_bit_depth() - 1)) + 1) as i32;
        for i in start..width as usize {
            let mut v = sp[i];
            if nlt_type == 4 {
                v = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(bias)
                };
            }
            let mut t = v as f32 * mul + 0.5; // convert to [0, 1]
            t = fmax(t, d_min);
            t = fmin(t, d_max);
            let k = floor_u32((t - d_min) * inv_delta);
            let d_k = d_min + k as f32 * delta;
            let t_k = lut[k as usize + 1];
            let t_kp1 = lut[k as usize + 2];
            let y = t_k + (t - d_k) * inv_delta * (t_kp1 - t_k);
            dp[i] = y - 0.5;
        }
    } else {
        for i in start..width as usize {
            let v = sp[i];
            let mut t = v as f32 * mul; // it is in [0, 1]
            t = fmax(t, d_min);
            t = fmin(t, d_max);
            let k = floor_u32((t - d_min) * inv_delta);
            let d_k = d_min + k as f32 * delta;
            let t_k = lut[k as usize + 1];
            let t_kp1 = lut[k as usize + 2];
            let y = t_k + (t - d_k) * inv_delta * (t_kp1 - t_k);
            dp[i] = y - 0.5;
        }
    }
}

/// Port of `gen_irv_convert_to_float_nlt`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:416-436 @ OpenJPH 0.32.0).
pub(crate) fn irv_convert_to_float_nlt(
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    bit_depth: u32,
    width: u32,
    rec: &NltRec,
) {
    if rec.get_type() == OJPH_NLT_LUT_STYLE_NLT {
        irv_convert_to_float_nlt2or4(
            2,
            src_line,
            src_line_offset,
            dst_line,
            bit_depth,
            width,
            rec,
        );
    } else if rec.get_type() == OJPH_NLT_BINARY_COMPLEMENT_PLUS_LUT {
        irv_convert_to_float_nlt2or4(
            4,
            src_line,
            src_line_offset,
            dst_line,
            bit_depth,
            width,
            rec,
        );
    }
}

/// Port of `gen_rev_convert` (external/OpenJPH/src/core/transform/ojph_colour.cpp:484-516
/// @ OpenJPH 0.32.0): integers shifted, between 32- and 64-bit lines.
pub(crate) fn rev_convert(
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    shift: i64,
    width: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::rev_convert(
            src_line,
            src_line_offset,
            dst_line,
            dst_line_offset,
            shift,
            width,
        )
    {
        return;
    }
    rev_convert_gen(
        src_line,
        src_line_offset,
        dst_line,
        dst_line_offset,
        shift,
        width,
    );
}

/// [`rev_convert`]'s generic kernel (the values of `avx2_rev_convert`, lines 70-139 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rev_convert_gen(
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    shift: i64,
    width: u32,
) {
    let (so, d_o, w) = (
        src_line_offset as usize,
        dst_line_offset as usize,
        width as usize,
    );
    if (src_line.flags & LFT_32BIT) != 0 {
        if (dst_line.flags & LFT_32BIT) != 0 {
            let sp = &src_line.i32()[so..so + w];
            let s = shift as i32;
            for (d, &v) in dst_line.i32_mut()[d_o..d_o + w].iter_mut().zip(sp) {
                *d = v.wrapping_add(s);
            }
        } else {
            let sp = &src_line.i32()[so..so + w];
            for (d, &v) in dst_line.i64_mut()[d_o..d_o + w].iter_mut().zip(sp) {
                *d = i64::from(v).wrapping_add(shift);
            }
        }
    } else {
        let sp = &src_line.i64()[so..so + w];
        for (d, &v) in dst_line.i32_mut()[d_o..d_o + w].iter_mut().zip(sp) {
            *d = v.wrapping_add(shift) as i32;
        }
    }
}

/// Port of `gen_rev_convert_nlt_type3`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:519-557 @ OpenJPH 0.32.0): the binary complement of negative integers (`-v - shift`).
pub(crate) fn rev_convert_nlt_type3(
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    shift: i64,
    width: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::rev_convert_nlt_type3(
            src_line,
            src_line_offset,
            dst_line,
            dst_line_offset,
            shift,
            width,
        )
    {
        return;
    }
    rev_convert_nlt_type3_gen(
        src_line,
        src_line_offset,
        dst_line,
        dst_line_offset,
        shift,
        width,
    );
}

/// [`rev_convert_nlt_type3`]'s generic kernel (the values of `avx2_rev_convert_nlt_type3`, lines 142-242 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rev_convert_nlt_type3_gen(
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    shift: i64,
    width: u32,
) {
    let (so, d_o, w) = (
        src_line_offset as usize,
        dst_line_offset as usize,
        width as usize,
    );
    if (src_line.flags & LFT_32BIT) != 0 {
        if (dst_line.flags & LFT_32BIT) != 0 {
            let sp = &src_line.i32()[so..so + w];
            let s = shift as i32;
            for (d, &v) in dst_line.i32_mut()[d_o..d_o + w].iter_mut().zip(sp) {
                *d = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(s)
                };
            }
        } else {
            let sp = &src_line.i32()[so..so + w];
            for (d, &v) in dst_line.i64_mut()[d_o..d_o + w].iter_mut().zip(sp) {
                let v = i64::from(v);
                *d = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(shift)
                };
            }
        }
    } else {
        let sp = &src_line.i64()[so..so + w];
        for (d, &v) in dst_line.i32_mut()[d_o..d_o + w].iter_mut().zip(sp) {
            *d = (if v >= 0 {
                v
            } else {
                v.wrapping_neg().wrapping_sub(shift)
            }) as i32;
        }
    }
}

/// Port of `local_gen_irv_convert_to_integer_nlt0or3`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:560-611 @ OpenJPH 0.32.0): floats to
/// integers of `bit_depth` bits, clamped, with the binary complement for type 3.
#[inline(always)]
fn irv_convert_to_integer_nlt0or3(
    nlt_type3: bool,
    src_line: &LineBuf,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    bit_depth: u32,
    is_signed: bool,
    width: u32,
) {
    let sp = src_line.f32();
    let dp = &mut dst_line.i32_mut()[dst_line_offset as usize..];
    let neg_limit = i32::MIN >> (32 - bit_depth);
    let mul = (1u64 << bit_depth) as f32;
    let fl_up_lim = -(neg_limit as f32); // val < upper
    let fl_low_lim = neg_limit as f32; // val >= lower
    let s32_up_lim = i32::MAX >> (32 - bit_depth);
    let s32_low_lim = i32::MIN >> (32 - bit_depth);
    let kernel = to_int_kernel();

    if is_signed {
        let bias = ((1u64 << (bit_depth - 1)) + 1) as i32;
        for i in 0..width as usize {
            let t = sp[i] * mul;
            let mut v = kernel.round(t);
            v = kernel.max_ge(v, s32_low_lim, t, fl_low_lim);
            v = kernel.min_lt(v, s32_up_lim, t, fl_up_lim);
            if nlt_type3 {
                v = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(bias)
                };
            }
            dp[i] = v;
        }
    } else {
        let half = (1u64 << (bit_depth - 1)) as i32;
        for i in 0..width as usize {
            let t = sp[i] * mul;
            let mut v = kernel.round(t);
            v = kernel.max_ge(v, s32_low_lim, t, fl_low_lim);
            v = kernel.min_lt(v, s32_up_lim, t, fl_up_lim);
            dp[i] = v.wrapping_add(half);
        }
    }
}

/// Port of `gen_irv_convert_to_integer`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:614-620 @ OpenJPH 0.32.0) and `gen_irv_convert_to_integer_nlt_type3` (lines 623-629).
pub(crate) fn irv_convert_to_integer(
    nlt_type3: bool,
    src_line: &LineBuf,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    bit_depth: u32,
    is_signed: bool,
    width: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::irv_convert_to_integer(
            nlt_type3,
            src_line,
            dst_line,
            dst_line_offset,
            bit_depth,
            is_signed,
            width,
        )
    {
        return;
    }
    irv_convert_to_integer_gen(
        nlt_type3,
        src_line,
        dst_line,
        dst_line_offset,
        bit_depth,
        is_signed,
        width,
    );
}

/// [`irv_convert_to_integer`]'s generic kernel (the values of `avx2_irv_convert_to_integer` and `avx2_irv_convert_to_integer_nlt_type3`, lines 271-351 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn irv_convert_to_integer_gen(
    nlt_type3: bool,
    src_line: &LineBuf,
    dst_line: &mut LineBuf,
    dst_line_offset: u32,
    bit_depth: u32,
    is_signed: bool,
    width: u32,
) {
    irv_convert_to_integer_nlt0or3(
        nlt_type3,
        src_line,
        dst_line,
        dst_line_offset,
        bit_depth,
        is_signed,
        width,
    );
}

/// Port of `local_gen_irv_convert_to_float_nlt0or3`
/// (external/OpenJPH/src/core/transform/ojph_colour.cpp:632-667 @ OpenJPH 0.32.0), with
/// `gen_irv_convert_to_float` (lines 670-676) and `gen_irv_convert_to_float_nlt_type3` (lines 679-685): integers of `bit_depth` bits to floats.
pub(crate) fn irv_convert_to_float(
    nlt_type3: bool,
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    bit_depth: u32,
    is_signed: bool,
    width: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::irv_convert_to_float(
            nlt_type3,
            src_line,
            src_line_offset,
            dst_line,
            bit_depth,
            is_signed,
            width,
        )
    {
        return;
    }
    irv_convert_to_float_gen(
        nlt_type3,
        src_line,
        src_line_offset,
        dst_line,
        bit_depth,
        is_signed,
        width,
    );
}

/// [`irv_convert_to_float`]'s generic kernel (the values of `avx2_irv_convert_to_float` and `avx2_irv_convert_to_float_nlt_type3`, lines 455-520 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn irv_convert_to_float_gen(
    nlt_type3: bool,
    src_line: &LineBuf,
    src_line_offset: u32,
    dst_line: &mut LineBuf,
    bit_depth: u32,
    is_signed: bool,
    width: u32,
) {
    let mul = (1.0 / (1u64 << bit_depth) as f64) as f32;
    let sp = &src_line.i32()[src_line_offset as usize..];
    let dp = dst_line.f32_mut();
    if is_signed {
        let bias = ((1u64 << (bit_depth - 1)) + 1) as i32;
        for i in 0..width as usize {
            let mut v = sp[i];
            if nlt_type3 {
                v = if v >= 0 {
                    v
                } else {
                    v.wrapping_neg().wrapping_sub(bias)
                };
            }
            dp[i] = v as f32 * mul;
        }
    } else {
        let half = (1u64 << (bit_depth - 1)) as i32;
        for i in 0..width as usize {
            let v = sp[i].wrapping_sub(half);
            dp[i] = v as f32 * mul;
        }
    }
}

/// Port of `gen_rct_forward` (external/OpenJPH/src/core/transform/ojph_colour.cpp:688-735
/// @ OpenJPH 0.32.0): the reversible colour transform, into 32- or 64-bit lines.
pub(crate) fn rct_forward(
    r: &LineBuf,
    g: &LineBuf,
    b: &LineBuf,
    y: &mut LineBuf,
    cb: &mut LineBuf,
    cr: &mut LineBuf,
    repeat: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::rct_forward(r, g, b, y, cb, cr, repeat)
    {
        return;
    }
    rct_forward_gen(r, g, b, y, cb, cr, repeat);
}

/// [`rct_forward`]'s generic kernel (the values of `avx2_rct_forward`, lines 620-707 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rct_forward_gen(
    r: &LineBuf,
    g: &LineBuf,
    b: &LineBuf,
    y: &mut LineBuf,
    cb: &mut LineBuf,
    cr: &mut LineBuf,
    repeat: u32,
) {
    let n = repeat as usize;
    let (rp, gp, bp) = (r.i32(), g.i32(), b.i32());
    if (y.flags & LFT_32BIT) != 0 {
        let (yp, cbp, crp) = (y.i32_mut(), cb.i32_mut(), cr.i32_mut());
        for i in 0..n {
            let (rr, gg, bb) = (rp[i], gp[i], bp[i]);
            yp[i] = rr.wrapping_add(gg << 1).wrapping_add(bb) >> 2;
            cbp[i] = bb.wrapping_sub(gg);
            crp[i] = rr.wrapping_sub(gg);
        }
    } else {
        let (yp, cbp, crp) = (y.i64_mut(), cb.i64_mut(), cr.i64_mut());
        for i in 0..n {
            let (rr, gg, bb) = (i64::from(rp[i]), i64::from(gp[i]), i64::from(bp[i]));
            yp[i] = (rr + (gg << 1) + bb) >> 2;
            cbp[i] = bb - gg;
            crp[i] = rr - gg;
        }
    }
}

/// Port of `gen_rct_backward` (external/OpenJPH/src/core/transform/ojph_colour.cpp:738-787
/// @ OpenJPH 0.32.0).
pub(crate) fn rct_backward(
    y: &LineBuf,
    cb: &LineBuf,
    cr: &LineBuf,
    r: &mut LineBuf,
    g: &mut LineBuf,
    b: &mut LineBuf,
    repeat: u32,
) {
    #[cfg(target_arch = "x86_64")]
    if get_cpu_ext_level() >= x86_level::AVX2
        && crate::colour_avx2::rct_backward(y, cb, cr, r, g, b, repeat)
    {
        return;
    }
    rct_backward_gen(y, cb, cr, r, g, b, repeat);
}

/// [`rct_backward`]'s generic kernel (the values of `avx2_rct_backward`, lines 710-820 of
/// ojph_colour_avx2.cpp, and of SSE2's), inlined into the AVX2 build too.
#[inline(always)]
pub(crate) fn rct_backward_gen(
    y: &LineBuf,
    cb: &LineBuf,
    cr: &LineBuf,
    r: &mut LineBuf,
    g: &mut LineBuf,
    b: &mut LineBuf,
    repeat: u32,
) {
    let n = repeat as usize;
    let (rp, gp, bp) = (r.i32_mut(), g.i32_mut(), b.i32_mut());
    if (y.flags & LFT_32BIT) != 0 {
        let (yp, cbp, crp) = (y.i32(), cb.i32(), cr.i32());
        for i in 0..n {
            let (yy, cbb, crr) = (yp[i], cbp[i], crp[i]);
            let gg = yy.wrapping_sub(cbb.wrapping_add(crr) >> 2);
            rp[i] = crr.wrapping_add(gg);
            gp[i] = gg;
            bp[i] = cbb.wrapping_add(gg);
        }
    } else {
        let (yp, cbp, crp) = (y.i64(), cb.i64(), cr.i64());
        for i in 0..n {
            let (yy, cbb, crr) = (yp[i], cbp[i], crp[i]);
            let gg = yy.wrapping_sub(cbb.wrapping_add(crr) >> 2);
            rp[i] = crr.wrapping_add(gg) as i32;
            gp[i] = gg as i32;
            bp[i] = cbb.wrapping_add(gg) as i32;
        }
    }
}

/// `CT_CNST` (external/OpenJPH/src/core/transform/ojph_colour.cpp:257-267 @ OpenJPH 0.32.0):
/// the irreversible colour transform's constants.
const ALPHA_RF: f32 = 0.299;
const ALPHA_GF: f32 = 0.587;
const ALPHA_BF: f32 = 0.114;
fn beta_cbf() -> f32 {
    (0.5 / (1.0 - f64::from(ALPHA_BF))) as f32
}
fn beta_crf() -> f32 {
    (0.5 / (1.0 - f64::from(ALPHA_RF))) as f32
}
fn gamma_cb2g() -> f32 {
    (2.0 * f64::from(ALPHA_BF) * (1.0 - f64::from(ALPHA_BF)) / f64::from(ALPHA_GF)) as f32
}
fn gamma_cr2g() -> f32 {
    (2.0 * f64::from(ALPHA_RF) * (1.0 - f64::from(ALPHA_RF)) / f64::from(ALPHA_GF)) as f32
}
fn gamma_cb2b() -> f32 {
    (2.0 * (1.0 - f64::from(ALPHA_BF))) as f32
}
fn gamma_cr2r() -> f32 {
    (2.0 * (1.0 - f64::from(ALPHA_RF))) as f32
}

/// Port of `gen_ict_forward` (external/OpenJPH/src/core/transform/ojph_colour.cpp:790-801
/// @ OpenJPH 0.32.0).
pub(crate) fn ict_forward(
    r: &[f32],
    g: &[f32],
    b: &[f32],
    y: &mut [f32],
    cb: &mut [f32],
    cr: &mut [f32],
    repeat: u32,
) {
    let (bcb, bcr) = (beta_cbf(), beta_crf());
    for i in 0..repeat as usize {
        y[i] = ALPHA_RF * r[i] + ALPHA_GF * g[i] + ALPHA_BF * b[i];
        cb[i] = bcb * (b[i] - y[i]);
        cr[i] = bcr * (r[i] - y[i]);
    }
}

/// Port of `gen_ict_backward` (external/OpenJPH/src/core/transform/ojph_colour.cpp:804-813
/// @ OpenJPH 0.32.0).
pub(crate) fn ict_backward(
    y: &[f32],
    cb: &[f32],
    cr: &[f32],
    r: &mut [f32],
    g: &mut [f32],
    b: &mut [f32],
    repeat: u32,
) {
    let (cr2g, cb2g, cr2r, cb2b) = (gamma_cr2g(), gamma_cb2g(), gamma_cr2r(), gamma_cb2b());
    for i in 0..repeat as usize {
        g[i] = y[i] - cr2g * cr[i] - cb2g * cb[i];
        r[i] = y[i] + cr2r * cr[i];
        b[i] = y[i] + cb2b * cb[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AVX2's clamps keep a lane as `vcmpps` with `_CMP_NLT_UQ` (`max_ge`) and `_CMP_NGE_UQ`
    /// (`min_lt`) decide, NaN included, on the CPU.
    #[test]
    fn avx2_clamps_compare_as_the_cpu() {
        let special = [
            0.0f32,
            -0.0,
            1.0,
            -1.0,
            0.5,
            f32::MIN_POSITIVE,
            f32::MAX,
            f32::MIN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            -f32::NAN,
            f32::from_bits(0x7f80_0001),
            f32::from_bits(0x0000_0001),
        ];
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for &x in &special {
            for &y in &special {
                a.push(x);
                b.push(y);
            }
        }
        let (nlt, nge) = openexr_testkit::cpu::vcmpps_nlt_nge_uq(&a, &b);
        for i in 0..a.len() {
            let (t, lim) = (a[i], b[i]);
            assert_eq!(
                ToIntKernel::Avx2.max_ge(1, 0, t, lim) == 1,
                nlt[i],
                "max_ge {t:?} {lim:?}"
            );
            assert_eq!(
                ToIntKernel::Avx2.min_lt(1, 0, t, lim) == 1,
                nge[i],
                "min_lt {t:?} {lim:?}"
            );
        }
    }

    fn line32(v: &[i32]) -> LineBuf {
        let mut l = LineBuf::new_i32(v.len(), 0);
        l.i32_mut()[..v.len()].copy_from_slice(v);
        l
    }

    /// The reversible colour transform reconstructs its input exactly, into 32- and 64-bit
    /// lines.
    #[test]
    fn rct_round_trip() {
        let n = 300usize;
        let r: Vec<i32> = (0..n as i32).map(|k| (k * 7919) % 65536 - 32768).collect();
        let g: Vec<i32> = (0..n as i32)
            .map(|k| (k * 104729) % 65536 - 32768)
            .collect();
        let b: Vec<i32> = (0..n as i32)
            .map(|k| (k * 1299709) % 65536 - 32768)
            .collect();
        let (rl, gl, bl) = (line32(&r), line32(&g), line32(&b));
        for wide in [false, true] {
            let mk = || {
                if wide {
                    LineBuf::new_i64(n, 0)
                } else {
                    LineBuf::new_i32(n, 0)
                }
            };
            let (mut y, mut cb, mut cr) = (mk(), mk(), mk());
            rct_forward(&rl, &gl, &bl, &mut y, &mut cb, &mut cr, n as u32);
            let (mut r2, mut g2, mut b2) =
                (line32(&[0; 300]), line32(&[0; 300]), line32(&[0; 300]));
            rct_backward(&y, &cb, &cr, &mut r2, &mut g2, &mut b2, n as u32);
            assert_eq!(&r2.i32()[..n], &r[..]);
            assert_eq!(&g2.i32()[..n], &g[..]);
            assert_eq!(&b2.i32()[..n], &b[..]);
        }
    }

    /// The binary complement of type 3 is its own inverse.
    #[test]
    fn nlt_type3_round_trip() {
        let v: Vec<i32> = (-200..200).map(|k| k * 163).collect();
        let src = line32(&v);
        let shift = (1i64 << 15) + 1;
        let mut mid = line32(&[0; 400]);
        rev_convert_nlt_type3(&src, 0, &mut mid, 0, shift, 400);
        let mut back = line32(&[0; 400]);
        rev_convert_nlt_type3(&mid, 0, &mut back, 0, shift, 400);
        assert_eq!(&back.i32()[..400], &v[..]);
    }
}
