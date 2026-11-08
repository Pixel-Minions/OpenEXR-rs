// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The ZSTD codec's AVX2 kernels, with the intrinsics: `shuffle_decode_4_avx2` and
//! `shuffle_decode_2_avx2` (src/lib/OpenEXRCore/internal_zstd_shuffle.c:79-161 @ v3.5.3), and
//! `delta_decode_row_u16_avx2` and `delta_decode_row_u32_avx2`
//! (src/lib/OpenEXRCore/internal_zstd_delta.c:91-217 @ v3.5.3).
//!
//! Upstream's GCC builds take them where `__builtin_cpu_supports ("avx2")`; MSVC's take the
//! scalar loops. They move the same bytes and add the same values modulo 2^16 and 2^32, so
//! the port takes them wherever the CPU has AVX2.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load and store
//! is of 32 bytes inside the slice it is given, and the kernels run only after the CPU
//! reported AVX2.
#![allow(unsafe_code)]

use std::arch::x86_64::{
    __m256i, _mm_extract_epi16, _mm_extract_epi32, _mm256_add_epi16, _mm256_add_epi32,
    _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_permute2x128_si256, _mm256_set_epi8,
    _mm256_set1_epi16, _mm256_set1_epi32, _mm256_shuffle_epi8, _mm256_shuffle_epi32,
    _mm256_slli_si256, _mm256_storeu_si256, _mm256_unpackhi_epi8, _mm256_unpackhi_epi16,
    _mm256_unpacklo_epi8, _mm256_unpacklo_epi16,
};

/// Whether the CPU has AVX2 (found once).
fn has_avx2() -> bool {
    use std::sync::OnceLock;
    static HAS: OnceLock<bool> = OnceLock::new();
    *HAS.get_or_init(|| std::arch::is_x86_feature_detected!("avx2"))
}

#[inline]
#[target_feature(enable = "avx2")]
fn load(b: &[u8]) -> __m256i {
    let b = &b[..32];
    // SAFETY: 32 bytes read from `b`.
    unsafe { _mm256_loadu_si256(b.as_ptr().cast()) }
}

#[inline]
#[target_feature(enable = "avx2")]
fn store(b: &mut [u8], v: __m256i) {
    let b = &mut b[..32];
    // SAFETY: 32 bytes written to `b`.
    unsafe { _mm256_storeu_si256(b.as_mut_ptr().cast(), v) }
}

/// `exr_zstd_shuffle_decode_bytes`'s AVX2 branch (src/lib/OpenEXRCore/internal_zstd_shuffle.c:
/// 275-305 @ v3.5.3): the planes of `shuf` into elements of `el_bytes` (4, or else 2) in
/// `dst`; false, having done nothing, without AVX2.
pub(crate) fn shuffle_decode_bytes(dst: &mut [u8], shuf: &[u8], el_bytes: u64) -> bool {
    if !has_avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2.
    unsafe {
        if el_bytes == 4 {
            shuffle_decode_4_avx2(shuf, dst);
        } else {
            shuffle_decode_2_avx2(shuf, dst);
        }
    }
    true
}

#[target_feature(enable = "avx2")]
fn shuffle_decode_4_avx2(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 4;
    let (p0, rest) = input.split_at(elements);
    let (p1, rest) = rest.split_at(elements);
    let (p2, p3) = rest.split_at(elements);
    let mut i = 0;

    if elements >= 32 {
        while i <= elements - 32 {
            let v0 = load(&p0[i..]);
            let v1 = load(&p1[i..]);
            let v2 = load(&p2[i..]);
            let v3 = load(&p3[i..]);

            let v01_lo = _mm256_unpacklo_epi8(v0, v1);
            let v01_hi = _mm256_unpackhi_epi8(v0, v1);
            let v23_lo = _mm256_unpacklo_epi8(v2, v3);
            let v23_hi = _mm256_unpackhi_epi8(v2, v3);

            let res0 = _mm256_unpacklo_epi16(v01_lo, v23_lo);
            let res1 = _mm256_unpackhi_epi16(v01_lo, v23_lo);
            let res2 = _mm256_unpacklo_epi16(v01_hi, v23_hi);
            let res3 = _mm256_unpackhi_epi16(v01_hi, v23_hi);

            let out0 = _mm256_permute2x128_si256::<0x20>(res0, res1);
            let out1 = _mm256_permute2x128_si256::<0x31>(res0, res1);
            let out2 = _mm256_permute2x128_si256::<0x20>(res2, res3);
            let out3 = _mm256_permute2x128_si256::<0x31>(res2, res3);

            store(&mut out[i * 4..], out0);
            store(&mut out[i * 4 + 32..], out2);
            store(&mut out[i * 4 + 64..], out1);
            store(&mut out[i * 4 + 96..], out3);
            i += 32;
        }
    }

    for i in i..elements {
        out[i * 4] = p0[i];
        out[i * 4 + 1] = p1[i];
        out[i * 4 + 2] = p2[i];
        out[i * 4 + 3] = p3[i];
    }
}

#[target_feature(enable = "avx2")]
fn shuffle_decode_2_avx2(input: &[u8], out: &mut [u8]) {
    let elements = input.len() / 2;
    let (p0, rest) = input.split_at(elements);
    let p1 = &rest[..elements];
    let mut i = 0;

    if elements >= 32 {
        while i <= elements - 32 {
            let v0 = load(&p0[i..]);
            let v1 = load(&p1[i..]);

            let lo = _mm256_unpacklo_epi8(v0, v1);
            let hi = _mm256_unpackhi_epi8(v0, v1);

            let out0 = _mm256_permute2x128_si256::<0x20>(lo, hi);
            let out1 = _mm256_permute2x128_si256::<0x31>(lo, hi);

            store(&mut out[i * 2..], out0);
            store(&mut out[i * 2 + 32..], out1);
            i += 32;
        }
    }

    for i in i..elements {
        out[i * 2] = p0[i];
        out[i * 2 + 1] = p1[i];
    }
}

/// `delta_decode_row_u16_avx2` (src/lib/OpenEXRCore/internal_zstd_delta.c:91-160 @ v3.5.3),
/// of the `n` 16-bit values at `p`; false, having done nothing, without AVX2.
pub(crate) fn delta_decode_row_u16(p: &mut [u8], n: u64) -> bool {
    if !has_avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2.
    unsafe { delta_decode_row_u16_avx2(p, n as usize) };
    true
}

#[target_feature(enable = "avx2")]
fn delta_decode_row_u16_avx2(p: &mut [u8], n: usize) {
    if n <= 1 {
        return;
    }
    let p = &mut p[..n * 2];
    let mut i = 1;
    let mut prev_sum = u16::from_le_bytes([p[0], p[1]]);

    #[rustfmt::skip]
    let bcast_mask = _mm256_set_epi8(
        15, 14, 15, 14, 15, 14, 15, 14, 15, 14, 15, 14, 15, 14, 15, 14,
        -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1, -1,
    );

    while i + 16 <= n {
        let mut x = load(&p[i * 2..]);
        x = _mm256_add_epi16(x, _mm256_slli_si256::<2>(x));
        x = _mm256_add_epi16(x, _mm256_slli_si256::<4>(x));
        x = _mm256_add_epi16(x, _mm256_slli_si256::<8>(x));

        let mut lane_shift = _mm256_permute2x128_si256::<0x08>(x, x);
        lane_shift = _mm256_shuffle_epi8(lane_shift, bcast_mask);
        x = _mm256_add_epi16(x, lane_shift);

        x = _mm256_add_epi16(x, _mm256_set1_epi16(prev_sum as i16));
        store(&mut p[i * 2..], x);

        let hi_lane = _mm256_extracti128_si256::<1>(x);
        prev_sum = _mm_extract_epi16::<7>(hi_lane) as u16;
        i += 16;
    }

    for k in i..n {
        let a = u16::from_le_bytes([p[k * 2], p[k * 2 + 1]]);
        let b = u16::from_le_bytes([p[(k - 1) * 2], p[(k - 1) * 2 + 1]]);
        p[k * 2..k * 2 + 2].copy_from_slice(&a.wrapping_add(b).to_le_bytes());
    }
}

/// `delta_decode_row_u32_avx2` (src/lib/OpenEXRCore/internal_zstd_delta.c:183-217 @ v3.5.3),
/// of the `n` 32-bit values at `p`; false, having done nothing, without AVX2.
pub(crate) fn delta_decode_row_u32(p: &mut [u8], n: u64) -> bool {
    if !has_avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2.
    unsafe { delta_decode_row_u32_avx2(p, n as usize) };
    true
}

#[target_feature(enable = "avx2")]
fn delta_decode_row_u32_avx2(p: &mut [u8], n: usize) {
    if n <= 1 {
        return;
    }
    let p = &mut p[..n * 4];
    let mut i = 1;
    let mut prev_sum = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);

    while i + 8 <= n {
        let mut x = load(&p[i * 4..]);
        x = _mm256_add_epi32(x, _mm256_slli_si256::<4>(x));
        x = _mm256_add_epi32(x, _mm256_slli_si256::<8>(x));

        let mut lane_shift = _mm256_permute2x128_si256::<0x08>(x, x);
        lane_shift = _mm256_shuffle_epi32::<0xff>(lane_shift);
        x = _mm256_add_epi32(x, lane_shift);

        x = _mm256_add_epi32(x, _mm256_set1_epi32(prev_sum as i32));
        store(&mut p[i * 4..], x);

        let hi_lane = _mm256_extracti128_si256::<1>(x);
        prev_sum = _mm_extract_epi32::<3>(hi_lane) as u32;
        i += 8;
    }

    for k in i..n {
        let a = u32::from_le_bytes(p[k * 4..k * 4 + 4].try_into().unwrap());
        let b = u32::from_le_bytes(p[(k - 1) * 4..k * 4].try_into().unwrap());
        p[k * 4..k * 4 + 4].copy_from_slice(&a.wrapping_add(b).to_le_bytes());
    }
}
