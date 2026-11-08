// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The F16C kernels Core's GCC builds take (no MSVC build has them): `vcvtps2ph $0` and
//! `vcvtph2ps`, eight values at a time, with the intrinsics, for DWA's
//! `convertFloatToHalf64_f16c` and `fromHalfZigZag_f16c`
//! (src/lib/OpenEXRCore/internal_dwa_simd.h:397-454, 623-803 @ v3.5.3) and unpacking's
//! `half_to_float_buffer_f16c` (src/lib/OpenEXRCore/unpack.c:25-81 @ v3.5.3).
//!
//! Each function runs the instructions only when the CPU has F16C and AVX and says whether it
//! did; the callers choose these kernels as upstream does and keep the emulations
//! (`internal_dwa_simd::vcvtph2ps` and `vcvtps2ph`, checked against the CPU) otherwise.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each load and store
//! is of a whole vector inside the slice it is given.
#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm_cvtph_ps, _mm_loadl_epi64, _mm_loadu_si128, _mm_storeu_ps, _mm_storeu_si128,
    _mm256_cvtph_ps, _mm256_cvtps_ph, _mm256_loadu_ps, _mm256_storeu_ps,
};

/// Whether the CPU has F16C and AVX (found once).
#[cfg(target_arch = "x86_64")]
fn has_f16c() -> bool {
    use std::sync::OnceLock;
    static HAS: OnceLock<bool> = OnceLock::new();
    *HAS.get_or_init(|| {
        std::arch::is_x86_feature_detected!("f16c") && std::arch::is_x86_feature_detected!("avx")
    })
}

/// `vcvtps2ph $0` of the 64 floats of `src` into `dst`, as `convertFloatToHalf64_f16c`'s eight
/// instructions do; false, having done nothing, without F16C.
pub(crate) fn float_to_half64(dst: &mut [u16; 64], src: &[f32; 64]) -> bool {
    #[cfg(target_arch = "x86_64")]
    if has_f16c() {
        // SAFETY: the CPU has F16C and AVX.
        unsafe { float_to_half64_f16c(dst, src) };
        return true;
    }
    let _ = (dst, src);
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,f16c")]
fn float_to_half64_f16c(dst: &mut [u16; 64], src: &[f32; 64]) {
    for (d, s) in dst
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<8>().0)
    {
        // SAFETY: eight floats read from `s`, eight halves written to `d`.
        unsafe {
            let h = _mm256_cvtps_ph::<0>(_mm256_loadu_ps(s.as_ptr()));
            _mm_storeu_si128(d.as_mut_ptr().cast(), h);
        }
    }
}

/// `vcvtph2ps` of the 64 halves of `src` into `dst`; false, having done nothing, without F16C.
pub(crate) fn half_to_float64(dst: &mut [f32; 64], src: &[u16; 64]) -> bool {
    #[cfg(target_arch = "x86_64")]
    if has_f16c() {
        // SAFETY: the CPU has F16C and AVX.
        unsafe { half_to_float64_f16c(dst, src) };
        return true;
    }
    let _ = (dst, src);
    false
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,f16c")]
fn half_to_float64_f16c(dst: &mut [f32; 64], src: &[u16; 64]) {
    for (d, s) in dst
        .as_chunks_mut::<8>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<8>().0)
    {
        // SAFETY: eight halves read from `s`, eight floats written to `d`.
        unsafe {
            let f = _mm256_cvtph_ps(_mm_loadu_si128(s.as_ptr().cast()));
            _mm256_storeu_ps(d.as_mut_ptr(), f);
        }
    }
}

/// `half_to_float_buffer_f16c`'s vector part: of the `n` halves at `inp` (native order, as
/// stored), eight at a time through `vcvtph2ps`, then four more when four to seven remain; the
/// floats (native order) at `out`. Returns how many it converted (the rest are the caller's,
/// through `half_to_float`), or `None` without F16C.
pub(crate) fn half_to_float_buffer(out: &mut [u8], inp: &[u8], n: usize) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    if has_f16c() {
        // SAFETY: the CPU has F16C and AVX.
        return Some(unsafe { half_to_float_buffer_f16c(out, inp, n) });
    }
    let _ = (out, inp, n);
    None
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,f16c")]
fn half_to_float_buffer_f16c(out: &mut [u8], inp: &[u8], n: usize) -> usize {
    let n = n.min(inp.len() / 2).min(out.len() / 4);
    let mut x = 0;
    while x + 8 <= n {
        let i = &inp[x * 2..x * 2 + 16];
        let o = &mut out[x * 4..x * 4 + 32];
        // SAFETY: 16 bytes read from `i`, 32 written to `o`.
        unsafe {
            let f = _mm256_cvtph_ps(_mm_loadu_si128(i.as_ptr().cast()));
            _mm256_storeu_ps(o.as_mut_ptr().cast(), f);
        }
        x += 8;
    }
    if n - x >= 4 {
        let i = &inp[x * 2..x * 2 + 8];
        let o = &mut out[x * 4..x * 4 + 16];
        // SAFETY: 8 bytes read from `i`, 16 written to `o`.
        unsafe {
            let f = _mm_cvtph_ps(_mm_loadl_epi64(i.as_ptr().cast()));
            _mm_storeu_ps(o.as_mut_ptr().cast(), f);
        }
        x += 4;
    }
    x
}
