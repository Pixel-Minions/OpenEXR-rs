// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! DWA's AVX inverse DCT (src/lib/OpenEXRCore/internal_dwa_simd.h:1360-1876 @ v3.5.3) with the
//! instructions themselves: the GCC inline assembly of `IDCT_AVX_BODY`, `dctInverse8x8_avx_0`
//! to `_6` and `dctInverse8x8_avx_7`, instruction for instruction, as intrinsics on `__m256`
//! registers. It is generated from the lane-by-lane emulation in `internal_dwa_simd.rs` (which
//! follows the assembly, and is checked against it through the wheel's DWA files): each
//! emulated register is a `__m256`, `add8`/`sub8`/`mul8` are `vaddps`/`vsubps`/`vmulps`,
//! `permil8` is `vpermilps`, `perm2f128` is `vperm2f128` and `broadcast128` is
//! `vbroadcastf128`. Core chooses this kernel where `check_for_x86_simd` reports AVX (its GCC
//! builds), and so do the callers.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load and store
//! is of eight floats inside the slice it is given, and the kernels run only after the CPU
//! reported AVX.
#![allow(unsafe_code)]

use std::arch::x86_64::{
    __m256, _mm_loadu_ps, _mm256_add_ps, _mm256_broadcast_ps, _mm256_mul_ps, _mm256_permute_ps,
    _mm256_permute2f128_ps, _mm256_set1_ps, _mm256_setr_ps, _mm256_storeu_ps, _mm256_sub_ps,
};

use crate::internal_dwa_simd::{AVX_COEF, Block};

/// The AVX inverse DCT of `data` with its last `zeroed_rows` rows 0 (`dctInverse8x8_avx_<n>`);
/// false, having done nothing, when the CPU lacks AVX.
pub(crate) fn dct_inverse_8x8(data: &mut Block, zeroed_rows: i32) -> bool {
    if !std::arch::is_x86_feature_detected!("avx") {
        return false;
    }
    // SAFETY: the CPU has AVX.
    unsafe {
        if zeroed_rows == 7 {
            avx_7(data);
        } else {
            avx_body(data, zeroed_rows);
        }
    }
    true
}

#[inline]
#[target_feature(enable = "avx")]
fn add(a: __m256, b: __m256) -> __m256 {
    _mm256_add_ps(a, b)
}

#[inline]
#[target_feature(enable = "avx")]
fn sub(a: __m256, b: __m256) -> __m256 {
    _mm256_sub_ps(a, b)
}

#[inline]
#[target_feature(enable = "avx")]
fn mul(a: __m256, b: __m256) -> __m256 {
    _mm256_mul_ps(a, b)
}

/// `vpermilps $IMM`.
#[inline]
#[target_feature(enable = "avx")]
fn permil<const IMM: i32>(a: __m256) -> __m256 {
    _mm256_permute_ps::<IMM>(a)
}

/// `vperm2f128 $IMM, src2, src1, dst` (AT&T order): lanes of `src1` (0, 1) or `src2` (2, 3).
#[inline]
#[target_feature(enable = "avx")]
fn p2f128<const IMM: i32>(src1: __m256, src2: __m256) -> __m256 {
    _mm256_permute2f128_ps::<IMM>(src1, src2)
}

/// `vbroadcastf128` of four floats.
#[inline]
#[target_feature(enable = "avx")]
fn bcast(v: &[f32]) -> __m256 {
    let v = &v[..4];
    // SAFETY: four floats read from `v`.
    unsafe { _mm256_broadcast_ps(&_mm_loadu_ps(v.as_ptr())) }
}

/// `vmovaps` of a register to eight floats.
#[inline]
#[target_feature(enable = "avx")]
fn store(out: &mut [f32], v: __m256) {
    let out = &mut out[..8];
    // SAFETY: eight floats written to `out`.
    unsafe { _mm256_storeu_ps(out.as_mut_ptr(), v) }
}

/// The 16 `ymm` registers of the AVX inverse DCT. Every register the assembly reads has been
/// written first; they start as NaN so that a read of an unwritten one could not go unseen.
struct Ymm([__m256; 16]);

impl Ymm {
    #[target_feature(enable = "avx")]
    fn new() -> Self {
        Ymm([_mm256_set1_ps(f32::NAN); 16])
    }

    /// `IDCT_AVX_SETUP_2_ROWS` (src/lib/OpenEXRCore/internal_dwa_simd.h:1371-1386
    /// @ v3.5.3): rows `2 * pair` and `2 * pair + 1`, the even columns of each into `dst0`
    /// and the odd ones into `dst1`, one row per lane.
    #[target_feature(enable = "avx")]
    fn setup_2_rows(&mut self, data: &Block, dst0: usize, dst1: usize, pair: usize) {
        let r0 = &data[16 * pair..16 * pair + 8];
        let r1 = &data[16 * pair + 8..16 * pair + 16];
        self.0[dst0] = _mm256_setr_ps(r0[0], r0[2], r0[4], r0[6], r1[0], r1[2], r1[4], r1[6]);
        self.0[dst1] = _mm256_setr_ps(r0[1], r0[3], r0[5], r0[7], r1[1], r1[3], r1[5], r1[7]);
    }

    /// `IDCT_AVX_MMULT_ROWS` (src/lib/OpenEXRCore/internal_dwa_simd.h:1388-1404 @ v3.5.3):
    /// `src = (x0 m0 + x1 m1) + (x2 m2 + x3 m3)`, with the matrix columns in ymm8-11.
    #[target_feature(enable = "avx")]
    fn mmult_rows(&mut self, src: usize) {
        let y = &mut self.0;
        y[12] = permil::<0x00>(y[src]);
        y[13] = permil::<0x55>(y[src]);
        y[14] = permil::<0xaa>(y[src]);
        y[15] = permil::<0xff>(y[src]);

        y[12] = mul(y[8], y[12]);
        y[13] = mul(y[9], y[13]);
        y[14] = mul(y[10], y[14]);
        y[15] = mul(y[11], y[15]);

        y[12] = add(y[12], y[13]);
        y[14] = add(y[14], y[15]);
        y[src] = add(y[12], y[14]);
    }

    /// `IDCT_AVX_EO_TO_ROW_HALVES` (src/lib/OpenEXRCore/internal_dwa_simd.h:1406-1410
    /// @ v3.5.3).
    #[target_feature(enable = "avx")]
    fn eo_to_row_halves(&mut self, even: usize, odd: usize, front: usize, back: usize) {
        let y = &mut self.0;
        y[back] = sub(y[even], y[odd]);
        y[front] = add(y[even], y[odd]);
        // Reverse the back half
        y[back] = permil::<0x1b>(y[back]);
    }
}

/// Port of `IDCT_AVX_BODY` and `dctInverse8x8_avx_0` to `_6`
/// (src/lib/OpenEXRCore/internal_dwa_simd.h:1421-1814 @ v3.5.3): the instructions of rows
/// `0..8 - zeroed_rows` (each wrapped in `ROWn ()` upstream), in order. Operands are named in
/// Intel order: `vsubps %%ymmA, %%ymmB, %%ymmC` is `C = B - A`.
#[target_feature(enable = "avx")]
fn avx_body(data: &mut Block, zeroed_rows: i32) {
    let has = |row: i32| row < 8 - zeroed_rows;
    let mut r = Ymm::new();

    // Row 1D DCT
    if has(0) {
        r.setup_2_rows(data, 0, 4, 0);
    }
    if has(2) {
        r.setup_2_rows(data, 1, 5, 1);
    }
    if has(4) {
        r.setup_2_rows(data, 2, 6, 2);
    }
    if has(6) {
        r.setup_2_rows(data, 3, 7, 3);
    }

    r.0[8] = bcast(&AVX_COEF[0..4]);
    r.0[9] = bcast(&AVX_COEF[4..8]);
    r.0[10] = bcast(&AVX_COEF[8..12]);
    r.0[11] = bcast(&AVX_COEF[12..16]);

    for (row, src) in [(0, 0), (2, 1), (4, 2), (6, 3)] {
        if has(row) {
            r.mmult_rows(src);
        }
    }

    r.0[8] = bcast(&AVX_COEF[16..20]);
    r.0[9] = bcast(&AVX_COEF[20..24]);
    r.0[10] = bcast(&AVX_COEF[24..28]);
    r.0[11] = bcast(&AVX_COEF[28..32]);

    for (row, src) in [(0, 4), (2, 5), (4, 6), (6, 7)] {
        if has(row) {
            r.mmult_rows(src);
        }
    }

    for (row, even, odd, back) in [(0, 0, 4, 12), (2, 1, 5, 13), (4, 2, 6, 14), (6, 3, 7, 15)] {
        if has(row) {
            r.eo_to_row_halves(even, odd, even, back);
        }
    }

    // Reassemble the rows halves into ymm0-7
    {
        let y = &mut r.0;
        if has(7) {
            y[7] = p2f128::<0x13>(y[15], y[3]);
        }
        if has(6) {
            y[6] = p2f128::<0x02>(y[15], y[3]);
        }
        if has(5) {
            y[5] = p2f128::<0x13>(y[14], y[2]);
        }
        if has(4) {
            y[4] = p2f128::<0x02>(y[14], y[2]);
        }
        if has(3) {
            y[3] = p2f128::<0x13>(y[13], y[1]);
        }
        if has(2) {
            y[2] = p2f128::<0x02>(y[13], y[1]);
        }
        if has(1) {
            y[1] = p2f128::<0x13>(y[12], y[0]);
        }
        if has(0) {
            y[0] = p2f128::<0x02>(y[12], y[0]);
        }
    }

    // Column 1D DCT
    let y = &mut r.0;
    y[11] = permil::<0xff>(y[8]);
    y[10] = permil::<0xaa>(y[8]);
    y[9] = permil::<0x55>(y[8]);
    y[8] = permil::<0x00>(y[8]);

    // ymm12 = b ymm1 + d ymm3 + e ymm5 + g ymm7
    if has(1) {
        y[12] = mul(y[8], y[1]);
    }
    if has(3) {
        y[13] = mul(y[9], y[3]);
    }
    if has(5) {
        y[14] = mul(y[10], y[5]);
    }
    if has(7) {
        y[15] = mul(y[11], y[7]);
    }
    if has(3) {
        y[12] = add(y[13], y[12]);
    }
    if has(7) {
        y[14] = add(y[15], y[14]);
    }
    if has(5) {
        y[12] = add(y[14], y[12]);
    }

    // ymm13 = d ymm1 - g ymm3 - b ymm5 - e ymm7
    if has(1) {
        y[13] = mul(y[9], y[1]);
    }
    if has(3) {
        y[14] = mul(y[11], y[3]);
    }
    if has(5) {
        y[15] = mul(y[8], y[5]);
    }
    if has(5) {
        y[14] = add(y[15], y[14]);
    }
    if has(3) {
        y[13] = sub(y[13], y[14]);
    }
    if has(7) {
        y[15] = mul(y[10], y[7]);
    }
    if has(7) {
        y[13] = sub(y[13], y[15]);
    }

    // ymm14 = e ymm1 - b ymm3 + g ymm5 + d ymm7
    if has(1) {
        y[14] = mul(y[10], y[1]);
    }
    if has(3) {
        y[15] = mul(y[8], y[3]);
    }
    if has(3) {
        y[14] = sub(y[14], y[15]);
    }
    if has(5) {
        y[15] = mul(y[11], y[5]);
    }
    if has(5) {
        y[14] = add(y[14], y[15]);
    }
    if has(7) {
        y[15] = mul(y[9], y[7]);
    }
    if has(7) {
        y[14] = add(y[14], y[15]);
    }

    // ymm15 = g ymm1 - e ymm3 + d ymm5 - b ymm7
    if has(1) {
        y[15] = mul(y[11], y[1]);
    }
    if has(3) {
        y[3] = mul(y[10], y[3]);
    }
    if has(5) {
        y[5] = mul(y[9], y[5]);
    }
    if has(7) {
        y[7] = mul(y[8], y[7]);
    }
    if has(5) {
        y[15] = add(y[5], y[15]);
    }
    if has(7) {
        y[3] = add(y[7], y[3]);
    }
    if has(3) {
        y[15] = sub(y[15], y[3]);
    }

    // Fill ymm1 with coef[2] = [ a a c f | a a c f ], then broadcast f, c and a
    y[1] = bcast(&AVX_COEF[2..6]);
    y[5] = permil::<0xff>(y[1]);
    y[3] = permil::<0xaa>(y[1]);
    y[1] = permil::<0x00>(y[1]);

    // ymm11 = (a ymm0 + a ymm4),   ymm1 = (a ymm0 - a ymm4)
    if has(0) {
        y[11] = mul(y[0], y[1]);
    }
    if has(4) {
        y[4] = mul(y[4], y[1]);
    }
    if has(0) {
        y[1] = y[11];
    }
    if has(4) {
        y[11] = add(y[11], y[4]);
    }
    if has(4) {
        y[1] = sub(y[1], y[4]);
    }

    // ymm7 = (c ymm2 + f ymm6)
    if has(2) {
        y[7] = mul(y[2], y[3]);
    }
    if has(6) {
        y[9] = mul(y[6], y[5]);
    }
    if has(6) {
        y[7] = add(y[7], y[9]);
    }

    // E_0 = ymm8 = (a ymm0 + a ymm4) + (c ymm2 + f ymm6)
    // E_3 = ymm11 = (a ymm0 + a ymm4) - (c ymm2 + f ymm6)
    if has(0) {
        y[8] = y[11];
    }
    if has(2) {
        y[8] = add(y[8], y[7]);
    }
    if has(2) {
        y[11] = sub(y[11], y[7]);
    }

    // ymm7 = (f ymm2 - c ymm6)
    if has(2) {
        y[7] = mul(y[2], y[5]);
    }
    if has(6) {
        y[9] = mul(y[6], y[3]);
    }
    if has(6) {
        y[7] = sub(y[7], y[9]);
    }

    // E_1 = ymm9 = (a ymm0 - a ymm4) + (f ymm2 - c ymm6)
    // E_2 = ymm10 = (a ymm0 - a ymm4) - (f ymm2 - c ymm6)
    if has(0) {
        y[9] = y[1];
        y[10] = y[1];
    }
    if has(2) {
        y[9] = add(y[1], y[7]);
    }
    if has(2) {
        y[10] = sub(y[1], y[7]);
    }

    // Add the even (ymm8-11) and the odds (ymm12-15), placing the results into ymm0-7
    y[0] = add(y[8], y[12]);
    y[1] = add(y[9], y[13]);
    y[2] = add(y[10], y[14]);
    y[3] = add(y[11], y[15]);

    y[7] = sub(y[8], y[12]);
    y[6] = sub(y[9], y[13]);
    y[5] = sub(y[10], y[14]);
    y[4] = sub(y[11], y[15]);

    for row in 0..8 {
        store(&mut data[8 * row..8 * row + 8], y[row]);
    }
}

/// Port of `dctInverse8x8_avx_7` (src/lib/OpenEXRCore/internal_dwa_simd.h:1816-1876
/// @ v3.5.3): rows 0 and 1 through the row DCT, then row 0 times `a` into every row.
#[target_feature(enable = "avx")]
fn avx_7(data: &mut Block) {
    let mut r = Ymm::new();
    r.setup_2_rows(data, 0, 4, 0);

    r.0[8] = bcast(&AVX_COEF[0..4]);
    r.0[9] = bcast(&AVX_COEF[4..8]);
    r.0[10] = bcast(&AVX_COEF[8..12]);
    r.0[11] = bcast(&AVX_COEF[12..16]);

    // Stash a vector of [a a a a | a a a a] away in ymm2
    r.0[2] = r.0[8];

    r.mmult_rows(0);

    r.0[8] = bcast(&AVX_COEF[16..20]);
    r.0[9] = bcast(&AVX_COEF[20..24]);
    r.0[10] = bcast(&AVX_COEF[24..28]);
    r.0[11] = bcast(&AVX_COEF[28..32]);

    r.mmult_rows(4);
    r.eo_to_row_halves(0, 4, 0, 12);

    let y = &mut r.0;
    y[0] = p2f128::<0x02>(y[12], y[0]);

    // DC only, so multiple by a and we're done
    y[0] = mul(y[0], y[2]);

    for row in 0..8 {
        store(&mut data[8 * row..8 * row + 8], y[0]);
    }
}
