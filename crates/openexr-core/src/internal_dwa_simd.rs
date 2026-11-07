// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_simd.h` (src/lib/OpenEXRCore/internal_dwa_simd.h:1-2400 @ v3.5.3):
//! DWA's block kernels (color conversion, byte interleaving, float and half conversion, the
//! forward and inverse 8x8 DCT) and their run-time dispatch.
//!
//! Each SIMD kernel upstream builds on x86-64 is reproduced in plain Rust, operation for
//! operation, in the order and grouping of its instructions, so that it gives the same bits
//! as the instructions (CLAUDE.md, "SIMD"). Rust's `f32` arithmetic is IEEE single precision
//! with round to nearest even, as SSE and AVX are under the default MXCSR, and Rust never
//! fuses a multiply and an add.
//!
//! - SSE2 is always compiled on x86-64 (`IMF_HAVE_SSE2`): `csc709Inverse64`,
//!   `interleaveByte2`, `dctInverse8x8DcOnly` and `dctForward8x8` are the SSE2 versions there.
//!   The SSE2 color conversion and DC-only kernels compute exactly what the scalar ones do,
//!   and the SSE2 interleave only moves bytes; the forward DCT and the SSE2 inverse DCT are
//!   emulated lane by lane, with their shuffles.
//! - The AVX inverse DCT and the F16C conversions are GCC inline assembly
//!   (`IMF_HAVE_GCC_INLINEASM_X86_64`), chosen at run time by `check_for_x86_simd`. MSVC has
//!   no such assembly and its `check_for_x86_simd` never reports AVX or F16C, so the Windows
//!   wheel takes the SSE2 inverse DCT and the scalar conversions (docs/baseline.md); the port
//!   does the same with an MSVC target.
//! - F16C's `vcvtph2ps` converts exactly but quiets a signaling NaN, and `vcvtps2ph` with
//!   immediate 0 rounds to nearest even and quiets NaNs; Imath's conversions keep a NaN's
//!   bits. The emulations below reproduce the instructions.

use std::sync::OnceLock;

use crate::internal_coding::{float_to_half, half_to_float};
use crate::internal_cpuid::check_for_x86_simd;

/// An 8x8 block of floats, row-major, as upstream's 32-byte aligned `float[64]`.
pub(crate) type Block = [f32; 64];

/// Port of `csc709Inverse` (src/lib/OpenEXRCore/internal_dwa_simd.h:99-111 @ v3.5.3): inverse
/// 709 color conversion, Y'CbCr to R'G'B', of one value.
pub(crate) fn csc709_inverse(comp0: &mut f32, comp1: &mut f32, comp2: &mut f32) {
    let src = [*comp0, *comp1, *comp2];

    *comp0 = src[0] + 1.5747f32 * src[2];
    *comp1 = src[0] - 0.1873f32 * src[1] - 0.4682f32 * src[2];
    *comp2 = src[0] + 1.8556f32 * src[1];
}

/// Port of the SSE2 `csc709Inverse64` (src/lib/OpenEXRCore/internal_dwa_simd.h:133-180
/// @ v3.5.3), lane by lane:
/// `r + b c0`, `((g c2) + y) + (b c3)` and `(c1 g) + y`, with `c2` and `c3` the negated
/// coefficients, which round as the scalar `y - 0.1873 g - 0.4682 b` does.
pub(crate) fn csc709_inverse64(comp0: &mut Block, comp1: &mut Block, comp2: &mut Block) {
    let c0 = 1.5747f32;
    let c1 = 1.8556f32;
    let c2 = -0.1873f32;
    let c3 = -0.4682f32;
    for i in 0..64 {
        let src0 = comp0[i];
        let src1 = comp1[i];
        let src2 = comp2[i];

        comp0[i] += src2 * c0;

        let mut g = comp1[i] * c2;
        let b = src2 * c3;
        g += src0;
        g += b;
        comp1[i] = g;

        comp2[i] = c1 * src1 + src0;
    }
}

/// Port of `csc709Forward64` (src/lib/OpenEXRCore/internal_dwa_simd.h:191-206 @ v3.5.3):
/// forward 709 color conversion, R'G'B' to Y'CbCr, of a block.
pub(crate) fn csc709_forward64(comp0: &mut Block, comp1: &mut Block, comp2: &mut Block) {
    for i in 0..64 {
        let src = [comp0[i], comp1[i], comp2[i]];

        comp0[i] = 0.2126f32 * src[0] + 0.7152f32 * src[1] + 0.0722f32 * src[2];
        comp1[i] = -0.1146f32 * src[0] - 0.3854f32 * src[1] + 0.5000f32 * src[2];
        comp2[i] = 0.5000f32 * src[0] - 0.4542f32 * src[1] - 0.0458f32 * src[2];
    }
}

/// Port of `interleaveByte2` (src/lib/OpenEXRCore/internal_dwa_simd.h:223-353 @ v3.5.3):
/// `dst = ABABAB...` from `src0 = AAA...` and `src1 = BBB...`, `numBytes` each. All three of
/// the SSE2 version's branches (aligned, half-aligned and unaligned) store the same bytes as
/// the scalar loop.
pub(crate) fn interleave_byte2(dst: &mut [u8], src0: &[u8], src1: &[u8], num_bytes: usize) {
    for x in 0..num_bytes {
        dst[2 * x] = src0[x];
        dst[2 * x + 1] = src1[x];
    }
}

/// Port of `convertFloatToHalf64_scalar` (src/lib/OpenEXRCore/internal_dwa_simd.h:369-374
/// @ v3.5.3).
fn convert_float_to_half64_scalar(dst: &mut [u16; 64], src: &Block) {
    for i in 0..64 {
        dst[i] = float_to_half(src[i]);
    }
}

/// `vcvtps2ph` with immediate 0 (round to nearest even) of one value: Imath's conversion,
/// which rounds to nearest even, except that a NaN is quieted (bit 9 set) instead of kept.
fn vcvtps2ph(f: f32) -> u16 {
    let ui = f.to_bits();
    if (ui & 0x7fff_ffff) > 0x7f80_0000 {
        let sign = ((ui >> 16) & 0x8000) as u16;
        return sign | 0x7e00 | ((ui & 0x007f_ffff) >> 13) as u16;
    }
    float_to_half(f)
}

/// `vcvtph2ps` of one value: exact, as Imath's conversion, except that a signaling NaN is
/// quieted (bit 22 set).
fn vcvtph2ps(h: u16) -> f32 {
    if (h & 0x7c00) == 0x7c00 && (h & 0x03ff) != 0 {
        let sign = u32::from(h & 0x8000) << 16;
        return f32::from_bits(sign | 0x7fc0_0000 | (u32::from(h & 0x03ff) << 13));
    }
    half_to_float(h)
}

/// Port of `convertFloatToHalf64_f16c` (src/lib/OpenEXRCore/internal_dwa_simd.h:397-454
/// @ v3.5.3): eight `vcvtps2ph $0` of eight floats each.
fn convert_float_to_half64_f16c(dst: &mut [u16; 64], src: &Block) {
    for i in 0..64 {
        dst[i] = vcvtps2ph(src[i]);
    }
}

/// The zig-zag order of `fromHalfZigZag`: `dst[i] = src[ZIG_ZAG[i]]`
/// (src/lib/OpenEXRCore/internal_dwa_simd.h:456-470 @ v3.5.3).
const ZIG_ZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11,
    18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34,
    37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];

/// Port of `fromHalfZigZag_scalar` (src/lib/OpenEXRCore/internal_dwa_simd.h:471-544 @ v3.5.3):
/// an 8x8 block of halves in zig-zag order to floats in normal order.
fn from_half_zig_zag_scalar(src: &[u16; 64], dst: &mut Block) {
    for i in 0..64 {
        dst[i] = half_to_float(src[ZIG_ZAG[i]]);
    }
}

/// Port of `fromHalfZigZag_f16c` (src/lib/OpenEXRCore/internal_dwa_simd.h:623-803 @ v3.5.3):
/// its shuffles reorder the block as the scalar version does, then `vcvtph2ps` converts it.
fn from_half_zig_zag_f16c(src: &[u16; 64], dst: &mut Block) {
    for i in 0..64 {
        dst[i] = vcvtph2ps(src[ZIG_ZAG[i]]);
    }
}

/// Port of the SSE2 `dctInverse8x8DcOnly` (src/lib/OpenEXRCore/internal_dwa_simd.h:877-885
/// @ v3.5.3), the same as the scalar one: the inverse DCT of a block whose AC terms are 0.
pub(crate) fn dct_inverse_8x8_dc_only(data: &mut Block) {
    let val = data[0] * 3.535536e-01f32 * 3.535536e-01f32;
    data.fill(val);
}

/// Port of `dctInverse8x8_scalar` (src/lib/OpenEXRCore/internal_dwa_simd.h:926-1028
/// @ v3.5.3), for the last `zeroed_rows` rows 0. Its constants come from the platform's
/// `cosf`.
fn dct_inverse_8x8_scalar(data: &mut Block, zeroed_rows: i32) {
    let a = 0.5f32 * (3.14159f32 / 4.0f32).cos();
    let b = 0.5f32 * (3.14159f32 / 16.0f32).cos();
    let c = 0.5f32 * (3.14159f32 / 8.0f32).cos();
    let d = 0.5f32 * (3.0f32 * 3.14159f32 / 16.0f32).cos();
    let e = 0.5f32 * (5.0f32 * 3.14159f32 / 16.0f32).cos();
    let f = 0.5f32 * (3.0f32 * 3.14159f32 / 8.0f32).cos();
    let g = 0.5f32 * (7.0f32 * 3.14159f32 / 16.0f32).cos();

    let mut alpha = [0f32; 4];
    let mut beta = [0f32; 4];
    let mut theta = [0f32; 4];
    let mut gamma = [0f32; 4];

    // First pass - row wise.
    for row in 0..(8 - zeroed_rows).max(0) as usize {
        let r = &mut data[row * 8..row * 8 + 8];

        alpha[0] = c * r[2];
        alpha[1] = f * r[2];
        alpha[2] = c * r[6];
        alpha[3] = f * r[6];

        beta[0] = b * r[1] + d * r[3] + e * r[5] + g * r[7];
        beta[1] = d * r[1] - g * r[3] - b * r[5] - e * r[7];
        beta[2] = e * r[1] - b * r[3] + g * r[5] + d * r[7];
        beta[3] = g * r[1] - e * r[3] + d * r[5] - b * r[7];

        theta[0] = a * (r[0] + r[4]);
        theta[3] = a * (r[0] - r[4]);

        theta[1] = alpha[0] + alpha[3];
        theta[2] = alpha[1] - alpha[2];

        gamma[0] = theta[0] + theta[1];
        gamma[1] = theta[3] + theta[2];
        gamma[2] = theta[3] - theta[2];
        gamma[3] = theta[0] - theta[1];

        r[0] = gamma[0] + beta[0];
        r[1] = gamma[1] + beta[1];
        r[2] = gamma[2] + beta[2];
        r[3] = gamma[3] + beta[3];

        r[4] = gamma[3] - beta[3];
        r[5] = gamma[2] - beta[2];
        r[6] = gamma[1] - beta[1];
        r[7] = gamma[0] - beta[0];
    }

    // Second pass - column wise.
    for column in 0..8 {
        alpha[0] = c * data[16 + column];
        alpha[1] = f * data[16 + column];
        alpha[2] = c * data[48 + column];
        alpha[3] = f * data[48 + column];

        beta[0] = b * data[8 + column]
            + d * data[24 + column]
            + e * data[40 + column]
            + g * data[56 + column];

        beta[1] = d * data[8 + column]
            - g * data[24 + column]
            - b * data[40 + column]
            - e * data[56 + column];

        beta[2] = e * data[8 + column] - b * data[24 + column]
            + g * data[40 + column]
            + d * data[56 + column];

        beta[3] = g * data[8 + column] - e * data[24 + column] + d * data[40 + column]
            - b * data[56 + column];

        theta[0] = a * (data[column] + data[32 + column]);
        theta[3] = a * (data[column] - data[32 + column]);

        theta[1] = alpha[0] + alpha[3];
        theta[2] = alpha[1] - alpha[2];

        gamma[0] = theta[0] + theta[1];
        gamma[1] = theta[3] + theta[2];
        gamma[2] = theta[3] - theta[2];
        gamma[3] = theta[0] - theta[1];

        data[column] = gamma[0] + beta[0];
        data[8 + column] = gamma[1] + beta[1];
        data[16 + column] = gamma[2] + beta[2];
        data[24 + column] = gamma[3] + beta[3];

        data[32 + column] = gamma[3] - beta[3];
        data[40 + column] = gamma[2] - beta[2];
        data[48 + column] = gamma[1] - beta[1];
        data[56 + column] = gamma[0] - beta[0];
    }
}

/// Four floats of an `__m128`.
type V4 = [f32; 4];

fn add4(a: V4, b: V4) -> V4 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2], a[3] + b[3]]
}

fn sub4(a: V4, b: V4) -> V4 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

fn mul4(a: V4, b: V4) -> V4 {
    [a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]]
}

fn splat4(v: f32) -> V4 {
    [v; 4]
}

/// `_mm_shuffle_ps (a, b, imm)`: two lanes of `a`, then two of `b`.
fn shuffle4(a: V4, b: V4, imm: u32) -> V4 {
    [
        a[(imm & 3) as usize],
        a[((imm >> 2) & 3) as usize],
        b[((imm >> 4) & 3) as usize],
        b[((imm >> 6) & 3) as usize],
    ]
}

/// The block as upstream's 16 `__m128`: vector `2 * row + half`.
fn load_vecs(data: &Block) -> [V4; 16] {
    let mut v = [[0f32; 4]; 16];
    for (i, chunk) in data.as_chunks::<4>().0.iter().enumerate() {
        v[i] = *chunk;
    }
    v
}

fn store_vecs(data: &mut Block, v: &[V4; 16]) {
    for (i, chunk) in data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        *chunk = v[i];
    }
}

/// Port of `dctInverse8x8_sse2` (src/lib/OpenEXRCore/internal_dwa_simd.h:1082-1313 @ v3.5.3),
/// for the last `zeroed_rows` rows 0: the rows as broadcasts times the matrix columns, summed
/// from a zeroed register, then four columns at a time.
fn dct_inverse_8x8_sse2(data: &mut Block, zeroed_rows: i32) {
    let a = splat4(3.535536e-01);
    let b = splat4(4.903927e-01);
    let c = splat4(4.619398e-01);
    let d = splat4(4.157349e-01);
    let e = splat4(2.777855e-01);
    let f = splat4(1.913422e-01);
    let g = splat4(9.754573e-02);

    let c0: V4 = [3.535536e-01, 3.535536e-01, 3.535536e-01, 3.535536e-01];
    let c1: V4 = [4.619398e-01, 1.913422e-01, -1.913422e-01, -4.619398e-01];
    let c2: V4 = [3.535536e-01, -3.535536e-01, -3.535536e-01, 3.535536e-01];
    let c3: V4 = [1.913422e-01, -4.619398e-01, 4.619398e-01, -1.913422e-01];

    let c4: V4 = [4.903927e-01, 4.157349e-01, 2.777855e-01, 9.754573e-02];
    let c5: V4 = [4.157349e-01, -9.754573e-02, -4.903927e-01, -2.777855e-01];
    let c6: V4 = [2.777855e-01, -4.903927e-01, 9.754573e-02, 4.157349e-01];
    let c7: V4 = [9.754573e-02, -2.777855e-01, 4.157349e-01, -4.903927e-01];

    let mut src_vec = load_vecs(data);

    // Rows: `DCT_INVERSE_8x8_SS2_ROW_LOOP`, for the rows the switch runs (`default` is 0).
    let rows = if (1..=7).contains(&zeroed_rows) {
        8 - zeroed_rows
    } else {
        8
    } as usize;
    for i in 0..rows {
        let lo = src_vec[2 * i];
        let hi = src_vec[2 * i + 1];
        let mut x = [
            shuffle4(lo, lo, 0x00),
            shuffle4(lo, lo, 0x55),
            shuffle4(lo, lo, 0xaa),
            shuffle4(lo, lo, 0xff),
            shuffle4(hi, hi, 0x00),
            shuffle4(hi, hi, 0x55),
            shuffle4(hi, hi, 0xaa),
            shuffle4(hi, hi, 0xff),
        ];

        x[0] = mul4(x[0], c0);
        x[2] = mul4(x[2], c1);
        x[4] = mul4(x[4], c2);
        x[6] = mul4(x[6], c3);

        x[1] = mul4(x[1], c4);
        x[3] = mul4(x[3], c5);
        x[5] = mul4(x[5], c6);
        x[7] = mul4(x[7], c7);

        let mut even_sum = splat4(0.0);
        even_sum = add4(even_sum, x[0]);
        even_sum = add4(even_sum, x[2]);
        even_sum = add4(even_sum, x[4]);
        even_sum = add4(even_sum, x[6]);

        let mut odd_sum = splat4(0.0);
        odd_sum = add4(odd_sum, x[1]);
        odd_sum = add4(odd_sum, x[3]);
        odd_sum = add4(odd_sum, x[5]);
        odd_sum = add4(odd_sum, x[7]);

        src_vec[2 * i] = add4(even_sum, odd_sum);
        let back = sub4(even_sum, odd_sum);
        src_vec[2 * i + 1] = shuffle4(back, back, 0x1b);
    }

    // Columns, four at a time, in two batches.
    for col in 0..2 {
        let mut input = [[0f32; 4]; 8];
        for i in 0..8 {
            input[i] = src_vec[2 * i + col];
        }

        let alpha = [
            mul4(c, input[2]),
            mul4(f, input[2]),
            mul4(c, input[6]),
            mul4(f, input[6]),
        ];

        let beta = [
            add4(
                add4(mul4(input[1], b), mul4(input[3], d)),
                add4(mul4(input[5], e), mul4(input[7], g)),
            ),
            sub4(
                sub4(mul4(input[1], d), mul4(input[3], g)),
                add4(mul4(input[5], b), mul4(input[7], e)),
            ),
            add4(
                sub4(mul4(input[1], e), mul4(input[3], b)),
                add4(mul4(input[5], g), mul4(input[7], d)),
            ),
            add4(
                sub4(mul4(input[1], g), mul4(input[3], e)),
                sub4(mul4(input[5], d), mul4(input[7], b)),
            ),
        ];

        let theta0 = mul4(a, add4(input[0], input[4]));
        let theta3 = mul4(a, sub4(input[0], input[4]));
        let theta1 = add4(alpha[0], alpha[3]);
        let theta2 = sub4(alpha[1], alpha[2]);

        let gamma = [
            add4(theta0, theta1),
            add4(theta3, theta2),
            sub4(theta3, theta2),
            sub4(theta0, theta1),
        ];

        src_vec[col] = add4(gamma[0], beta[0]);
        src_vec[2 + col] = add4(gamma[1], beta[1]);
        src_vec[4 + col] = add4(gamma[2], beta[2]);
        src_vec[6 + col] = add4(gamma[3], beta[3]);

        src_vec[8 + col] = sub4(gamma[3], beta[3]);
        src_vec[10 + col] = sub4(gamma[2], beta[2]);
        src_vec[12 + col] = sub4(gamma[1], beta[1]);
        src_vec[14 + col] = sub4(gamma[0], beta[0]);
    }

    store_vecs(data, &src_vec);
}

/// Eight floats of a `ymm` register: two 128-bit lanes.
type V8 = [f32; 8];

fn add8(a: V8, b: V8) -> V8 {
    std::array::from_fn(|i| a[i] + b[i])
}

fn sub8(a: V8, b: V8) -> V8 {
    std::array::from_fn(|i| a[i] - b[i])
}

fn mul8(a: V8, b: V8) -> V8 {
    std::array::from_fn(|i| a[i] * b[i])
}

/// `vpermilps $imm`: the same four-way selection in each 128-bit lane.
fn permil8(a: V8, imm: u32) -> V8 {
    std::array::from_fn(|i| {
        let lane = i & !3;
        a[lane + ((imm >> (2 * (i & 3))) & 3) as usize]
    })
}

/// `vbroadcastf128`: four floats into both lanes.
fn broadcast128(v: &[f32]) -> V8 {
    [v[0], v[1], v[2], v[3], v[0], v[1], v[2], v[3]]
}

/// `vperm2f128 $imm, src2, src1, dst` (AT&T operand order) without the zeroing bits: each
/// lane of the result is a lane of `src1` (0, 1) or `src2` (2, 3).
fn perm2f128(src1: V8, src2: V8, imm: u32) -> V8 {
    let pick = |sel: u32| -> [f32; 4] {
        let s = if sel & 2 != 0 { &src2 } else { &src1 };
        let o = if sel & 1 != 0 { 4 } else { 0 };
        [s[o], s[o + 1], s[o + 2], s[o + 3]]
    };
    let lo = pick(imm & 3);
    let hi = pick((imm >> 4) & 3);
    [lo[0], lo[1], lo[2], lo[3], hi[0], hi[1], hi[2], hi[3]]
}

/// `sAvxCoef` (src/lib/OpenEXRCore/internal_dwa_simd.h:1698-1716 @ v3.5.3): M1 then M2, each
/// column-major.
const AVX_COEF: [f32; 32] = [
    3.535536e-01,
    3.535536e-01,
    3.535536e-01,
    3.535536e-01, // a  a  a  a
    4.619398e-01,
    1.913422e-01,
    -1.913422e-01,
    -4.619398e-01, // c  f -f -c
    3.535536e-01,
    -3.535536e-01,
    -3.535536e-01,
    3.535536e-01, // a -a -a  a
    1.913422e-01,
    -4.619398e-01,
    4.619398e-01,
    -1.913422e-01, // f -c  c -f
    4.903927e-01,
    4.157349e-01,
    2.777855e-01,
    9.754573e-02, // b  d  e  g
    4.157349e-01,
    -9.754573e-02,
    -4.903927e-01,
    -2.777855e-01, // d -g -b -e
    2.777855e-01,
    -4.903927e-01,
    9.754573e-02,
    4.157349e-01, // e -b  g  d
    9.754573e-02,
    -2.777855e-01,
    4.157349e-01,
    -4.903927e-01, // g -e  d -b
];

/// The 16 `ymm` registers of the AVX inverse DCT. Every register the assembly reads has been
/// written first; they start as NaN so that a read of an unwritten one could not go unseen.
struct Ymm([V8; 16]);

impl Ymm {
    fn new() -> Self {
        Ymm([[f32::NAN; 8]; 16])
    }

    /// `IDCT_AVX_SETUP_2_ROWS` (src/lib/OpenEXRCore/internal_dwa_simd.h:1371-1386
    /// @ v3.5.3): rows `2 * pair` and `2 * pair + 1`, the even columns of each into `dst0`
    /// and the odd ones into `dst1`, one row per lane.
    fn setup_2_rows(&mut self, data: &Block, dst0: usize, dst1: usize, pair: usize) {
        let r0 = &data[16 * pair..16 * pair + 8];
        let r1 = &data[16 * pair + 8..16 * pair + 16];
        self.0[dst0] = [r0[0], r0[2], r0[4], r0[6], r1[0], r1[2], r1[4], r1[6]];
        self.0[dst1] = [r0[1], r0[3], r0[5], r0[7], r1[1], r1[3], r1[5], r1[7]];
    }

    /// `IDCT_AVX_MMULT_ROWS` (src/lib/OpenEXRCore/internal_dwa_simd.h:1388-1404 @ v3.5.3):
    /// `src = (x0 m0 + x1 m1) + (x2 m2 + x3 m3)`, with the matrix columns in ymm8-11.
    fn mmult_rows(&mut self, src: usize) {
        let y = &mut self.0;
        y[12] = permil8(y[src], 0x00);
        y[13] = permil8(y[src], 0x55);
        y[14] = permil8(y[src], 0xaa);
        y[15] = permil8(y[src], 0xff);

        y[12] = mul8(y[8], y[12]);
        y[13] = mul8(y[9], y[13]);
        y[14] = mul8(y[10], y[14]);
        y[15] = mul8(y[11], y[15]);

        y[12] = add8(y[12], y[13]);
        y[14] = add8(y[14], y[15]);
        y[src] = add8(y[12], y[14]);
    }

    /// `IDCT_AVX_EO_TO_ROW_HALVES` (src/lib/OpenEXRCore/internal_dwa_simd.h:1406-1410
    /// @ v3.5.3).
    fn eo_to_row_halves(&mut self, even: usize, odd: usize, front: usize, back: usize) {
        let y = &mut self.0;
        y[back] = sub8(y[even], y[odd]);
        y[front] = add8(y[even], y[odd]);
        // Reverse the back half
        y[back] = permil8(y[back], 0x1b);
    }
}

/// Port of `IDCT_AVX_BODY` and `dctInverse8x8_avx_0` to `_6`
/// (src/lib/OpenEXRCore/internal_dwa_simd.h:1421-1814 @ v3.5.3): the instructions of rows
/// `0..8 - zeroed_rows` (each wrapped in `ROWn ()` upstream), in order. Operands are named in
/// Intel order: `vsubps %%ymmA, %%ymmB, %%ymmC` is `C = B - A`.
fn dct_inverse_8x8_avx(data: &mut Block, zeroed_rows: i32) {
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

    r.0[8] = broadcast128(&AVX_COEF[0..4]);
    r.0[9] = broadcast128(&AVX_COEF[4..8]);
    r.0[10] = broadcast128(&AVX_COEF[8..12]);
    r.0[11] = broadcast128(&AVX_COEF[12..16]);

    for (row, src) in [(0, 0), (2, 1), (4, 2), (6, 3)] {
        if has(row) {
            r.mmult_rows(src);
        }
    }

    r.0[8] = broadcast128(&AVX_COEF[16..20]);
    r.0[9] = broadcast128(&AVX_COEF[20..24]);
    r.0[10] = broadcast128(&AVX_COEF[24..28]);
    r.0[11] = broadcast128(&AVX_COEF[28..32]);

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
            y[7] = perm2f128(y[15], y[3], 0x13);
        }
        if has(6) {
            y[6] = perm2f128(y[15], y[3], 0x02);
        }
        if has(5) {
            y[5] = perm2f128(y[14], y[2], 0x13);
        }
        if has(4) {
            y[4] = perm2f128(y[14], y[2], 0x02);
        }
        if has(3) {
            y[3] = perm2f128(y[13], y[1], 0x13);
        }
        if has(2) {
            y[2] = perm2f128(y[13], y[1], 0x02);
        }
        if has(1) {
            y[1] = perm2f128(y[12], y[0], 0x13);
        }
        if has(0) {
            y[0] = perm2f128(y[12], y[0], 0x02);
        }
    }

    // Column 1D DCT
    let y = &mut r.0;
    y[11] = permil8(y[8], 0xff);
    y[10] = permil8(y[8], 0xaa);
    y[9] = permil8(y[8], 0x55);
    y[8] = permil8(y[8], 0x00);

    // ymm12 = b ymm1 + d ymm3 + e ymm5 + g ymm7
    if has(1) {
        y[12] = mul8(y[8], y[1]);
    }
    if has(3) {
        y[13] = mul8(y[9], y[3]);
    }
    if has(5) {
        y[14] = mul8(y[10], y[5]);
    }
    if has(7) {
        y[15] = mul8(y[11], y[7]);
    }
    if has(3) {
        y[12] = add8(y[13], y[12]);
    }
    if has(7) {
        y[14] = add8(y[15], y[14]);
    }
    if has(5) {
        y[12] = add8(y[14], y[12]);
    }

    // ymm13 = d ymm1 - g ymm3 - b ymm5 - e ymm7
    if has(1) {
        y[13] = mul8(y[9], y[1]);
    }
    if has(3) {
        y[14] = mul8(y[11], y[3]);
    }
    if has(5) {
        y[15] = mul8(y[8], y[5]);
    }
    if has(5) {
        y[14] = add8(y[15], y[14]);
    }
    if has(3) {
        y[13] = sub8(y[13], y[14]);
    }
    if has(7) {
        y[15] = mul8(y[10], y[7]);
    }
    if has(7) {
        y[13] = sub8(y[13], y[15]);
    }

    // ymm14 = e ymm1 - b ymm3 + g ymm5 + d ymm7
    if has(1) {
        y[14] = mul8(y[10], y[1]);
    }
    if has(3) {
        y[15] = mul8(y[8], y[3]);
    }
    if has(3) {
        y[14] = sub8(y[14], y[15]);
    }
    if has(5) {
        y[15] = mul8(y[11], y[5]);
    }
    if has(5) {
        y[14] = add8(y[14], y[15]);
    }
    if has(7) {
        y[15] = mul8(y[9], y[7]);
    }
    if has(7) {
        y[14] = add8(y[14], y[15]);
    }

    // ymm15 = g ymm1 - e ymm3 + d ymm5 - b ymm7
    if has(1) {
        y[15] = mul8(y[11], y[1]);
    }
    if has(3) {
        y[3] = mul8(y[10], y[3]);
    }
    if has(5) {
        y[5] = mul8(y[9], y[5]);
    }
    if has(7) {
        y[7] = mul8(y[8], y[7]);
    }
    if has(5) {
        y[15] = add8(y[5], y[15]);
    }
    if has(7) {
        y[3] = add8(y[7], y[3]);
    }
    if has(3) {
        y[15] = sub8(y[15], y[3]);
    }

    // Fill ymm1 with coef[2] = [ a a c f | a a c f ], then broadcast f, c and a
    y[1] = broadcast128(&AVX_COEF[2..6]);
    y[5] = permil8(y[1], 0xff);
    y[3] = permil8(y[1], 0xaa);
    y[1] = permil8(y[1], 0x00);

    // ymm11 = (a ymm0 + a ymm4),   ymm1 = (a ymm0 - a ymm4)
    if has(0) {
        y[11] = mul8(y[0], y[1]);
    }
    if has(4) {
        y[4] = mul8(y[4], y[1]);
    }
    if has(0) {
        y[1] = y[11];
    }
    if has(4) {
        y[11] = add8(y[11], y[4]);
    }
    if has(4) {
        y[1] = sub8(y[1], y[4]);
    }

    // ymm7 = (c ymm2 + f ymm6)
    if has(2) {
        y[7] = mul8(y[2], y[3]);
    }
    if has(6) {
        y[9] = mul8(y[6], y[5]);
    }
    if has(6) {
        y[7] = add8(y[7], y[9]);
    }

    // E_0 = ymm8 = (a ymm0 + a ymm4) + (c ymm2 + f ymm6)
    // E_3 = ymm11 = (a ymm0 + a ymm4) - (c ymm2 + f ymm6)
    if has(0) {
        y[8] = y[11];
    }
    if has(2) {
        y[8] = add8(y[8], y[7]);
    }
    if has(2) {
        y[11] = sub8(y[11], y[7]);
    }

    // ymm7 = (f ymm2 - c ymm6)
    if has(2) {
        y[7] = mul8(y[2], y[5]);
    }
    if has(6) {
        y[9] = mul8(y[6], y[3]);
    }
    if has(6) {
        y[7] = sub8(y[7], y[9]);
    }

    // E_1 = ymm9 = (a ymm0 - a ymm4) + (f ymm2 - c ymm6)
    // E_2 = ymm10 = (a ymm0 - a ymm4) - (f ymm2 - c ymm6)
    if has(0) {
        y[9] = y[1];
        y[10] = y[1];
    }
    if has(2) {
        y[9] = add8(y[1], y[7]);
    }
    if has(2) {
        y[10] = sub8(y[1], y[7]);
    }

    // Add the even (ymm8-11) and the odds (ymm12-15), placing the results into ymm0-7
    y[0] = add8(y[8], y[12]);
    y[1] = add8(y[9], y[13]);
    y[2] = add8(y[10], y[14]);
    y[3] = add8(y[11], y[15]);

    y[7] = sub8(y[8], y[12]);
    y[6] = sub8(y[9], y[13]);
    y[5] = sub8(y[10], y[14]);
    y[4] = sub8(y[11], y[15]);

    for row in 0..8 {
        data[8 * row..8 * row + 8].copy_from_slice(&y[row]);
    }
}

/// Port of `dctInverse8x8_avx_7` (src/lib/OpenEXRCore/internal_dwa_simd.h:1816-1876
/// @ v3.5.3): rows 0 and 1 through the row DCT, then row 0 times `a` into every row.
fn dct_inverse_8x8_avx_7(data: &mut Block) {
    let mut r = Ymm::new();
    r.setup_2_rows(data, 0, 4, 0);

    r.0[8] = broadcast128(&AVX_COEF[0..4]);
    r.0[9] = broadcast128(&AVX_COEF[4..8]);
    r.0[10] = broadcast128(&AVX_COEF[8..12]);
    r.0[11] = broadcast128(&AVX_COEF[12..16]);

    // Stash a vector of [a a a a | a a a a] away in ymm2
    r.0[2] = r.0[8];

    r.mmult_rows(0);

    r.0[8] = broadcast128(&AVX_COEF[16..20]);
    r.0[9] = broadcast128(&AVX_COEF[20..24]);
    r.0[10] = broadcast128(&AVX_COEF[24..28]);
    r.0[11] = broadcast128(&AVX_COEF[28..32]);

    r.mmult_rows(4);
    r.eo_to_row_halves(0, 4, 0, 12);

    let y = &mut r.0;
    y[0] = perm2f128(y[12], y[0], 0x02);

    // DC only, so multiple by a and we're done
    y[0] = mul8(y[0], y[2]);

    for row in 0..8 {
        data[8 * row..8 * row + 8].copy_from_slice(&y[0]);
    }
}

/// Port of the SSE2 `dctForward8x8` (src/lib/OpenEXRCore/internal_dwa_simd.h:2108-2306
/// @ v3.5.3): the 1D DCT on four columns at a time, then a transpose by 4x4 blocks, twice.
pub(crate) fn dct_forward_8x8(data: &mut Block) {
    let mut src_vec = load_vecs(data);

    let c4_vec = splat4(0.70710678);
    let c4_neg_vec = splat4(-0.70710678);

    let c1_half_vec = splat4(0.490392640);
    let c2_half_vec = splat4(0.461939770);
    let c3_half_vec = splat4(0.415734810);
    let c5_half_vec = splat4(0.277785120);
    let c6_half_vec = splat4(0.191341720);
    let c7_half_vec = splat4(0.097545161);

    let half_vec = splat4(0.5);

    for _iter in 0..2 {
        for pass in 0..2 {
            let a0 = add4(src_vec[pass], src_vec[14 + pass]);
            let a1 = add4(src_vec[2 + pass], src_vec[4 + pass]);
            let a3 = add4(src_vec[6 + pass], src_vec[8 + pass]);
            let a5 = add4(src_vec[10 + pass], src_vec[12 + pass]);

            let a7 = sub4(src_vec[pass], src_vec[14 + pass]);
            let a2 = sub4(src_vec[2 + pass], src_vec[4 + pass]);
            let a4 = sub4(src_vec[6 + pass], src_vec[8 + pass]);
            let a6 = sub4(src_vec[10 + pass], src_vec[12 + pass]);

            // First stage; Compute out_0 and out_4
            let mut k0 = add4(a0, a3);
            let mut k1 = add4(a1, a5);

            k0 = mul4(c4_vec, k0);
            k1 = mul4(c4_vec, k1);

            src_vec[pass] = add4(k0, k1);
            src_vec[8 + pass] = sub4(k0, k1);

            src_vec[pass] = mul4(src_vec[pass], half_vec);
            src_vec[8 + pass] = mul4(src_vec[8 + pass], half_vec);

            // Second stage; Compute out_2 and out_6
            k0 = sub4(a2, a6);
            k1 = sub4(a0, a3);

            src_vec[4 + pass] = add4(mul4(c6_half_vec, k0), mul4(c2_half_vec, k1));
            src_vec[12 + pass] = sub4(mul4(c6_half_vec, k1), mul4(c2_half_vec, k0));

            // Precompute K0 and K1 for the remaining stages
            k0 = mul4(sub4(a1, a5), c4_vec);
            k1 = mul4(add4(a2, a6), c4_neg_vec);

            // Third Stage, compute out_3 and out_5
            let rot_x = sub4(a7, k0);
            let rot_y = add4(a4, k1);

            src_vec[6 + pass] = sub4(mul4(c3_half_vec, rot_x), mul4(c5_half_vec, rot_y));
            src_vec[10 + pass] = add4(mul4(c5_half_vec, rot_x), mul4(c3_half_vec, rot_y));

            // Fourth Stage, compute out_1 and out_7
            let rot_x = add4(a7, k0);
            let rot_y = sub4(k1, a4);

            src_vec[2 + pass] = sub4(mul4(c1_half_vec, rot_x), mul4(c7_half_vec, rot_y));
            src_vec[14 + pass] = add4(mul4(c7_half_vec, rot_x), mul4(c1_half_vec, rot_y));
        }

        // Transpose the matrix, in 4x4 blocks.
        let s = &mut src_vec;
        let mut t = [[0f32; 4]; 4];
        let mut t2 = [[0f32; 4]; 4];

        // M0t, done in place, the first half.
        t[0] = shuffle4(s[0], s[2], 0x44);
        t[1] = shuffle4(s[4], s[6], 0x44);
        t[3] = shuffle4(s[4], s[6], 0xEE);
        t[2] = shuffle4(s[0], s[2], 0xEE);

        // M3t, also done in place, the first half.
        t2[0] = shuffle4(s[9], s[11], 0x44);
        t2[1] = shuffle4(s[13], s[15], 0x44);
        t2[2] = shuffle4(s[9], s[11], 0xEE);
        t2[3] = shuffle4(s[13], s[15], 0xEE);

        // M0t, the second half.
        s[0] = shuffle4(t[0], t[1], 0x88);
        s[4] = shuffle4(t[2], t[3], 0x88);
        s[2] = shuffle4(t[0], t[1], 0xDD);
        s[6] = shuffle4(t[2], t[3], 0xDD);

        // M3t, the second half.
        s[9] = shuffle4(t2[0], t2[1], 0x88);
        s[13] = shuffle4(t2[2], t2[3], 0x88);
        s[11] = shuffle4(t2[0], t2[1], 0xDD);
        s[15] = shuffle4(t2[2], t2[3], 0xDD);

        // First, the first half of M1t
        t[0] = shuffle4(s[1], s[3], 0x44);
        t[1] = shuffle4(s[5], s[7], 0x44);
        t[2] = shuffle4(s[1], s[3], 0xEE);
        t[3] = shuffle4(s[5], s[7], 0xEE);

        // And the first half of M2t
        t2[0] = shuffle4(s[8], s[10], 0x44);
        t2[1] = shuffle4(s[12], s[14], 0x44);
        t2[2] = shuffle4(s[8], s[10], 0xEE);
        t2[3] = shuffle4(s[12], s[14], 0xEE);

        // Second half of M1t
        s[8] = shuffle4(t[0], t[1], 0x88);
        s[12] = shuffle4(t[2], t[3], 0x88);
        s[10] = shuffle4(t[0], t[1], 0xDD);
        s[14] = shuffle4(t[2], t[3], 0xDD);

        // Second half of M2
        s[1] = shuffle4(t2[0], t2[1], 0x88);
        s[5] = shuffle4(t2[2], t2[3], 0x88);
        s[3] = shuffle4(t2[0], t2[1], 0xDD);
        s[7] = shuffle4(t2[2], t2[3], 0xDD);
    }

    store_vecs(data, &src_vec);
}

/// Which inverse DCT `initializeFuncs` chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InverseDct {
    Scalar,
    Sse2,
    Avx,
}

/// The function pointers of `initializeFuncs`: `convertFloatToHalf64`, `fromHalfZigZag` and
/// `dctInverse8x8_0` to `_7`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DwaFuncs {
    f16c: bool,
    inverse_dct: InverseDct,
}

impl DwaFuncs {
    /// `convertFloatToHalf64`.
    pub(crate) fn convert_float_to_half64(&self, dst: &mut [u16; 64], src: &Block) {
        if self.f16c {
            convert_float_to_half64_f16c(dst, src);
        } else {
            convert_float_to_half64_scalar(dst, src);
        }
    }

    /// `fromHalfZigZag`.
    #[allow(clippy::wrong_self_convention)] // upstream's name
    pub(crate) fn from_half_zig_zag(&self, src: &[u16; 64], dst: &mut Block) {
        if self.f16c {
            from_half_zig_zag_f16c(src, dst);
        } else {
            from_half_zig_zag_scalar(src, dst);
        }
    }

    /// `dctInverse8x8_<zeroed_rows>` (0 to 7).
    pub(crate) fn dct_inverse_8x8(&self, data: &mut Block, zeroed_rows: i32) {
        match self.inverse_dct {
            InverseDct::Scalar => dct_inverse_8x8_scalar(data, zeroed_rows),
            InverseDct::Sse2 => dct_inverse_8x8_sse2(data, zeroed_rows),
            InverseDct::Avx if zeroed_rows == 7 => dct_inverse_8x8_avx_7(data),
            InverseDct::Avx => dct_inverse_8x8_avx(data, zeroed_rows),
        }
    }
}

/// Port of `initializeFuncs` (src/lib/OpenEXRCore/internal_dwa_simd.h:2340-2400 @ v3.5.3),
/// once per process.
pub(crate) fn initialize_funcs() -> DwaFuncs {
    static FUNCS: OnceLock<DwaFuncs> = OnceLock::new();
    *FUNCS.get_or_init(|| {
        let simd = check_for_x86_simd();
        let f16c = simd.avx != 0 && simd.f16c != 0;
        let inverse_dct = if simd.avx != 0 {
            InverseDct::Avx
        } else if simd.sse2 != 0 {
            InverseDct::Sse2
        } else {
            InverseDct::Scalar
        };
        DwaFuncs { f16c, inverse_dct }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use openexr_testkit::rand48::Rand48;

    /// Blocks of random halves (as the decoder makes them), with the last rows zeroed.
    fn blocks(seed: u64, zeroed_rows: usize) -> Vec<Block> {
        let mut rng = Rand48::new(seed);
        (0..200)
            .map(|_| {
                let mut b = [0f32; 64];
                for (i, v) in b.iter_mut().enumerate() {
                    if i < 8 * (8 - zeroed_rows) {
                        // finite halves only: exponents below 31
                        let h = (rng.nexti() as u16) & 0xfbff;
                        *v = half_to_float(h);
                    }
                }
                b
            })
            .collect()
    }

    #[test]
    fn avx_inverse_dct_reads_only_what_it_wrote() {
        for zeroed in 0..8 {
            for block in blocks(7 + zeroed as u64, zeroed) {
                let mut data = block;
                if zeroed == 7 {
                    dct_inverse_8x8_avx_7(&mut data);
                } else {
                    dct_inverse_8x8_avx(&mut data, zeroed as i32);
                }
                assert!(
                    data.iter().all(|v| !v.is_nan()),
                    "zeroed {zeroed}: a NaN from an unwritten register"
                );
            }
        }
    }
}
