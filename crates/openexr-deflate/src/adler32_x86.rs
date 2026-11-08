// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `adler32_template.h` (external/deflate/lib/x86/adler32_template.h:1-518
//! @ libdeflate v1.26) without VNNI (`USE_VNNI` 0, `USE_AVX512` 0), for `VL` 16 (SSE2) and 32
//! (AVX2), with the choice of `arch_select_adler32_func` between them
//! (external/deflate/lib/x86/adler32_impl.h:106-131 @ libdeflate v1.26).
//!
//! Every variant computes the Adler-32 checksum itself, so the variant only changes the speed;
//! the AVX-VNNI and AVX-512 VNNI ones are in `adler32_vnni.rs`. The
//! pointer alignment step for long inputs (the template's lines 245-253) only aligns loads and
//! is left out as well.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load is of a
//! whole vector inside the slice, and each `target_feature` function is called only after the
//! CPU reported the feature.
#![allow(unsafe_code)]

use std::arch::x86_64::{
    __m128i, __m256i, _mm_add_epi16, _mm_add_epi32, _mm_cvtsi128_si32, _mm_loadu_si128,
    _mm_madd_epi16, _mm_sad_epu8, _mm_setr_epi16, _mm_setzero_si128, _mm_shuffle_epi32,
    _mm_slli_epi32, _mm_unpackhi_epi8, _mm_unpacklo_epi8, _mm256_add_epi16, _mm256_add_epi32,
    _mm256_castsi256_si128, _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_madd_epi16,
    _mm256_sad_epu8, _mm256_setr_epi16, _mm256_setzero_si256, _mm256_slli_epi32,
    _mm256_unpackhi_epi8, _mm256_unpacklo_epi8,
};

use crate::adler32::{MAX_CHUNK_LEN, adler32_chunk};

/// The checksum of `p` continuing `adler`, by the vector variant `arch_select_adler32_func`
/// chooses: a VNNI one (`adler32_vnni`) where the CPU has VNNI, else AVX2, else SSE2.
pub(crate) fn adler32(adler: u32, p: &[u8]) -> u32 {
    if let Some(v) = crate::adler32_vnni::adler32(adler, p) {
        return v;
    }
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU has AVX2.
        unsafe { adler32_avx2(adler, p) }
    } else {
        // SAFETY: SSE2 is part of x86-64.
        unsafe { adler32_sse2(adler, p) }
    }
}

/// `reduce_to_32bits` for 128-bit vectors (the template's lines 150-180): the 32-bit elements
/// of `v_s1` (its odd ones are zero, from `psadbw`) and of `v_s2`, added to s1 and s2.
#[target_feature(enable = "sse2")]
fn reduce_128(v_s1: __m128i, v_s2: __m128i, s1: &mut u32, s2: &mut u32) {
    let v_s2 = _mm_add_epi32(v_s2, _mm_shuffle_epi32::<0x31>(v_s2));
    let v_s1 = _mm_add_epi32(v_s1, _mm_shuffle_epi32::<0x02>(v_s1));
    let v_s2 = _mm_add_epi32(v_s2, _mm_shuffle_epi32::<0x02>(v_s2));
    *s1 = s1.wrapping_add(_mm_cvtsi128_si32(v_s1) as u32);
    *s2 = s2.wrapping_add(_mm_cvtsi128_si32(v_s2) as u32);
}

/// `adler32_x86_sse2` (the template with `VL` 16, lines 182-512).
#[target_feature(enable = "sse2")]
fn adler32_sse2(adler: u32, mut p: &[u8]) -> u32 {
    const VL: usize = 16;
    const LOG2_VL: i32 = 4;
    // the 16-bit values [2*VL, 2*VL - 1, ..., 1]
    let mults_a = _mm_setr_epi16(32, 31, 30, 29, 28, 27, 26, 25);
    let mults_b = _mm_setr_epi16(24, 23, 22, 21, 20, 19, 18, 17);
    let mults_c = _mm_setr_epi16(16, 15, 14, 13, 12, 11, 10, 9);
    let mults_d = _mm_setr_epi16(8, 7, 6, 5, 4, 3, 2, 1);
    let zeroes = _mm_setzero_si128();
    let mut s1 = adler & 0xFFFF;
    let mut s2 = adler >> 16;

    while !p.is_empty() {
        // s1 and s2 stay within u32, and each byte counter within i16 (pmaddwd is signed)
        let n = p.len().min(
            (2 * VL * (i16::MAX as usize / u8::MAX as usize)).min(MAX_CHUNK_LEN) & !(2 * VL - 1),
        );
        let (mut chunk, rest) = p.split_at(n);
        p = rest;
        if n >= 2 * VL {
            let mut v_s1 = zeroes;
            let mut v_s1_sums = zeroes;
            let (mut sa, mut sb, mut sc, mut sd) = (zeroes, zeroes, zeroes, zeroes);
            s2 = s2.wrapping_add(s1.wrapping_mul((n & !(2 * VL - 1)) as u32));
            while chunk.len() >= 2 * VL {
                // SAFETY: both loads are of 16 bytes inside `chunk`.
                let (data_a, data_b) = unsafe {
                    (
                        _mm_loadu_si128(chunk.as_ptr().cast()),
                        _mm_loadu_si128(chunk[VL..].as_ptr().cast()),
                    )
                };
                v_s1_sums = _mm_add_epi32(v_s1_sums, v_s1);
                sa = _mm_add_epi16(sa, _mm_unpacklo_epi8(data_a, zeroes));
                sb = _mm_add_epi16(sb, _mm_unpackhi_epi8(data_a, zeroes));
                sc = _mm_add_epi16(sc, _mm_unpacklo_epi8(data_b, zeroes));
                sd = _mm_add_epi16(sd, _mm_unpackhi_epi8(data_b, zeroes));
                v_s1 = _mm_add_epi32(
                    v_s1,
                    _mm_add_epi32(_mm_sad_epu8(data_a, zeroes), _mm_sad_epu8(data_b, zeroes)),
                );
                chunk = &chunk[2 * VL..];
            }
            // (2*VL)*v_s1_sums + [2*VL, ..., 1] * v_byte_sums
            let v_s2 = _mm_add_epi32(
                _mm_slli_epi32::<{ LOG2_VL + 1 }>(v_s1_sums),
                _mm_add_epi32(
                    _mm_add_epi32(_mm_madd_epi16(sa, mults_a), _mm_madd_epi16(sb, mults_b)),
                    _mm_add_epi32(_mm_madd_epi16(sc, mults_c), _mm_madd_epi16(sd, mults_d)),
                ),
            );
            reduce_128(v_s1, v_s2, &mut s1, &mut s2);
        }
        // the last 0 <= n < 2*VL bytes of the chunk, and s1 and s2 reduced mod DIVISOR
        adler32_chunk(&mut s1, &mut s2, chunk);
    }
    (s2 << 16) | s1
}

/// `adler32_x86_avx2` (the template with `VL` 32, lines 182-512).
#[target_feature(enable = "avx2")]
fn adler32_avx2(adler: u32, mut p: &[u8]) -> u32 {
    const VL: usize = 32;
    const LOG2_VL: i32 = 5;
    // [2*VL, ..., 1], in the order vpunpck{l,h}bw take bytes from each 128-bit lane
    let mults_a = _mm256_setr_epi16(
        64, 63, 62, 61, 60, 59, 58, 57, 48, 47, 46, 45, 44, 43, 42, 41,
    );
    let mults_b = _mm256_setr_epi16(
        56, 55, 54, 53, 52, 51, 50, 49, 40, 39, 38, 37, 36, 35, 34, 33,
    );
    let mults_c = _mm256_setr_epi16(
        32, 31, 30, 29, 28, 27, 26, 25, 16, 15, 14, 13, 12, 11, 10, 9,
    );
    let mults_d = _mm256_setr_epi16(24, 23, 22, 21, 20, 19, 18, 17, 8, 7, 6, 5, 4, 3, 2, 1);
    let zeroes = _mm256_setzero_si256();
    let mut s1 = adler & 0xFFFF;
    let mut s2 = adler >> 16;

    while !p.is_empty() {
        let n = p.len().min(
            (2 * VL * (i16::MAX as usize / u8::MAX as usize)).min(MAX_CHUNK_LEN) & !(2 * VL - 1),
        );
        let (mut chunk, rest) = p.split_at(n);
        p = rest;
        if n >= 2 * VL {
            let mut v_s1: __m256i = zeroes;
            let mut v_s1_sums = zeroes;
            let (mut sa, mut sb, mut sc, mut sd) = (zeroes, zeroes, zeroes, zeroes);
            s2 = s2.wrapping_add(s1.wrapping_mul((n & !(2 * VL - 1)) as u32));
            while chunk.len() >= 2 * VL {
                // SAFETY: both loads are of 32 bytes inside `chunk`.
                let (data_a, data_b) = unsafe {
                    (
                        _mm256_loadu_si256(chunk.as_ptr().cast()),
                        _mm256_loadu_si256(chunk[VL..].as_ptr().cast()),
                    )
                };
                v_s1_sums = _mm256_add_epi32(v_s1_sums, v_s1);
                sa = _mm256_add_epi16(sa, _mm256_unpacklo_epi8(data_a, zeroes));
                sb = _mm256_add_epi16(sb, _mm256_unpackhi_epi8(data_a, zeroes));
                sc = _mm256_add_epi16(sc, _mm256_unpacklo_epi8(data_b, zeroes));
                sd = _mm256_add_epi16(sd, _mm256_unpackhi_epi8(data_b, zeroes));
                v_s1 = _mm256_add_epi32(
                    v_s1,
                    _mm256_add_epi32(
                        _mm256_sad_epu8(data_a, zeroes),
                        _mm256_sad_epu8(data_b, zeroes),
                    ),
                );
                chunk = &chunk[2 * VL..];
            }
            let v_s2 = _mm256_add_epi32(
                _mm256_slli_epi32::<{ LOG2_VL + 1 }>(v_s1_sums),
                _mm256_add_epi32(
                    _mm256_add_epi32(
                        _mm256_madd_epi16(sa, mults_a),
                        _mm256_madd_epi16(sb, mults_b),
                    ),
                    _mm256_add_epi32(
                        _mm256_madd_epi16(sc, mults_c),
                        _mm256_madd_epi16(sd, mults_d),
                    ),
                ),
            );
            // reduce 256 bits to 128, then as the 128-bit variant
            let s1_128 = _mm_add_epi32(
                _mm256_castsi256_si128(v_s1),
                _mm256_extracti128_si256::<1>(v_s1),
            );
            let s2_128 = _mm_add_epi32(
                _mm256_castsi256_si128(v_s2),
                _mm256_extracti128_si256::<1>(v_s2),
            );
            reduce_128(s1_128, s2_128, &mut s1, &mut s2);
        }
        adler32_chunk(&mut s1, &mut s2, chunk);
    }
    (s2 << 16) | s1
}

/// The AVX2 and SSE2 variants' sums of `data` this CPU can compute, for the heavy test.
#[cfg(all(test, feature = "heavy"))]
pub(crate) fn variants(data: &[u8]) -> Vec<(&'static str, u32)> {
    let mut v = Vec::new();
    if std::arch::is_x86_feature_detected!("avx2") {
        // SAFETY: the CPU has AVX2.
        v.push(("avx2", unsafe { adler32_avx2(1, data) }));
    }
    // SAFETY: SSE2 is part of x86-64.
    v.push(("sse2", unsafe { adler32_sse2(1, data) }));
    v
}
