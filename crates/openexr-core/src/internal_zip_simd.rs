// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! ZIP's x86-64 kernels: upstream's SSE4.1 `reconstruct` and SSE2 `interleave`
//! (src/lib/OpenEXRCore/internal_zip.c:49-94, 161-190 @ v3.5.3), with the intrinsics.
//!
//! Upstream compiles them when `IMF_HAVE_SSE4_1` and `IMF_HAVE_SSE2` are defined: always under
//! MSVC on x86-64 (the Windows wheel), and under GCC only for `__SSE4_1__` and `__SSE2__`, so
//! the Linux wheel takes the SSE2 interleave and the scalar reconstruct. Both compute the
//! scalar versions' bytes (a prefix sum of bytes modulo 256, a byte interleave), so the port
//! takes them wherever the CPU has the instructions.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load and store
//! is of 16 bytes inside the slice it is given, and each `target_feature` function is called
//! only after the CPU reported the feature.
#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    __m128i, _mm_add_epi8, _mm_extract_epi8, _mm_loadu_si128, _mm_set1_epi8, _mm_setzero_si128,
    _mm_shuffle_epi8, _mm_slli_si128, _mm_storeu_si128, _mm_unpackhi_epi8, _mm_unpacklo_epi8,
};

/// Port of the SSE4.1 `reconstruct` (src/lib/OpenEXRCore/internal_zip.c:49-94 @ v3.5.3), when
/// the CPU has SSSE3 and SSE4.1; returns false, having done nothing, otherwise.
pub(crate) fn reconstruct(buf: &mut [u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("ssse3") && std::arch::is_x86_feature_detected!("sse4.1")
    {
        // SAFETY: the CPU has the features the function is compiled for.
        unsafe { reconstruct_sse41(buf) };
        return true;
    }
    let _ = buf;
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "ssse3,sse4.1")]
fn reconstruct_sse41(buf: &mut [u8]) {
    let out_size = buf.len();
    if out_size == 0 {
        // (upstream pre-flips buf[0] whatever the size; a zero-sized chunk has none)
        return;
    }
    let v_out_size = out_size / 16;
    let c = _mm_set1_epi8(-128);
    let shuffle_mask = _mm_set1_epi8(15);

    // The first element doesn't have its high bit flipped during compression, so it must not
    // be flipped here. To make the SIMD loop nice and uniform, we pre-flip the bit so that
    // the loop will unflip it again.
    buf[0] = buf[0].wrapping_add(128);
    let mut v_prev = _mm_setzero_si128();

    for i in 0..v_out_size {
        let p = buf[i * 16..i * 16 + 16].as_mut_ptr().cast::<__m128i>();
        // SAFETY: `p` points at 16 bytes of `buf`.
        let mut d = _mm_add_epi8(unsafe { _mm_loadu_si128(p) }, c);

        // Compute the prefix sum of elements.
        d = _mm_add_epi8(d, _mm_slli_si128::<1>(d));
        d = _mm_add_epi8(d, _mm_slli_si128::<2>(d));
        d = _mm_add_epi8(d, _mm_slli_si128::<4>(d));
        d = _mm_add_epi8(d, _mm_slli_si128::<8>(d));
        d = _mm_add_epi8(d, v_prev);

        // SAFETY: as the load.
        unsafe { _mm_storeu_si128(p, d) };

        // Broadcast the high byte in our result to all lanes of the prev value for the next
        // iteration.
        v_prev = _mm_shuffle_epi8(d, shuffle_mask);
    }

    let mut prev = _mm_extract_epi8::<15>(v_prev) as u8;
    for b in &mut buf[v_out_size * 16..] {
        let d = prev.wrapping_add(*b).wrapping_sub(128);
        *b = d;
        prev = d;
    }
}

/// Port of the SSE2 `interleave` (src/lib/OpenEXRCore/internal_zip.c:161-190 @ v3.5.3): the
/// first half of `source` into the even bytes of `out`, the second into the odd ones.
pub(crate) fn interleave(out: &mut [u8], source: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is part of x86-64.
        unsafe { interleave_sse2(out, source) };
        true
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (out, source);
        false
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
fn interleave_sse2(out: &mut [u8], source: &[u8]) {
    let out_size = out.len();
    let half = out_size.div_ceil(2);
    let (s1, s2) = source[..out_size].split_at(half);
    let v_out_size = out_size / 32;

    for i in 0..v_out_size {
        let a_src = &s1[i * 16..i * 16 + 16];
        let b_src = &s2[i * 16..i * 16 + 16];
        let dst = &mut out[i * 32..i * 32 + 32];
        // SAFETY: each pointer points at 16 bytes of its slice (`dst` holds 32).
        unsafe {
            let a = _mm_loadu_si128(a_src.as_ptr().cast());
            let b = _mm_loadu_si128(b_src.as_ptr().cast());
            let lo = _mm_unpacklo_epi8(a, b);
            let hi = _mm_unpackhi_epi8(a, b);
            _mm_storeu_si128(dst.as_mut_ptr().cast(), lo);
            _mm_storeu_si128(dst[16..].as_mut_ptr().cast(), hi);
        }
    }

    let (mut t1, mut t2) = (v_out_size * 16, v_out_size * 16);
    for (i, o) in out.iter_mut().enumerate().skip(v_out_size * 32) {
        if i % 2 == 0 {
            *o = s1[t1];
            t1 += 1;
        } else {
            *o = s2[t2];
            t2 += 1;
        }
    }
}
