// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The CPU's own instructions, called through `std::arch`: the reference that the port's safe
//! emulations of upstream's SIMD paths are checked against (CLAUDE.md rule 2, "the CPU for
//! instruction semantics").
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8, owner 2026-11-07:
//! a test-only CPU probe). Every wrapper is safe: it checks that the CPU has the instruction
//! set it needs (and panics if not), and moves values through local arrays of the
//! instruction's width, so no load or store reaches past a caller's slice.
#![allow(unsafe_code)]

#[cfg(not(target_arch = "x86_64"))]
compile_error!("the CPU reference supports x86_64 only");

use std::arch::x86_64::{
    __m128i, __m256, _CMP_NGE_UQ, _CMP_NLT_UQ, _mm_cvtps_epi32, _mm_cvtsi32_si128,
    _mm_cvtsi64_si128, _mm_cvtsi128_si32, _mm_cvtsi128_si64, _mm_cvtt_ss2si, _mm_loadu_ps,
    _mm_loadu_si128, _mm_set_ss, _mm_sll_epi64, _mm_sra_epi32, _mm_srl_epi64, _mm_storeu_si128,
    _mm256_cmp_ps, _mm256_cvtph_ps, _mm256_cvtps_ph, _mm256_loadu_ps, _mm256_movemask_ps,
    _mm256_storeu_ps,
};

/// Whether the CPU has F16C, with the AVX state it needs.
pub fn has_f16c() -> bool {
    std::arch::is_x86_feature_detected!("f16c") && std::arch::is_x86_feature_detected!("avx")
}

/// Whether the CPU has AVX.
pub fn has_avx() -> bool {
    std::arch::is_x86_feature_detected!("avx")
}

fn need(present: bool, what: &str) {
    assert!(present, "this check needs a CPU with {what}");
}

/// `vcvtph2ps` (`_mm256_cvtph_ps`) of each half of `input`, eight at a time.
pub fn vcvtph2ps(input: &[u16]) -> Vec<f32> {
    need(has_f16c(), "F16C");
    let mut out = Vec::with_capacity(input.len());
    for block in input.chunks(8) {
        let mut src = [0u16; 8];
        src[..block.len()].copy_from_slice(block);
        // SAFETY: the CPU has F16C and AVX (checked above).
        let dst = unsafe { cvtph_ps8(&src) };
        out.extend_from_slice(&dst[..block.len()]);
    }
    out
}

#[target_feature(enable = "avx,f16c")]
fn cvtph_ps8(src: &[u16; 8]) -> [f32; 8] {
    let mut dst = [0f32; 8];
    // SAFETY: both arrays hold the eight lanes the instructions read and write.
    unsafe {
        let v = _mm_loadu_si128(src.as_ptr().cast::<__m128i>());
        _mm256_storeu_ps(dst.as_mut_ptr(), _mm256_cvtph_ps(v));
    }
    dst
}

/// `vcvtps2ph` with immediate 0 (round to nearest even; `_mm256_cvtps_ph`) of each float of
/// `input`, eight at a time.
pub fn vcvtps2ph(input: &[f32]) -> Vec<u16> {
    need(has_f16c(), "F16C");
    let mut out = Vec::with_capacity(input.len());
    for block in input.chunks(8) {
        let mut src = [0f32; 8];
        src[..block.len()].copy_from_slice(block);
        // SAFETY: the CPU has F16C and AVX (checked above).
        let dst = unsafe { cvtps_ph8(&src) };
        out.extend_from_slice(&dst[..block.len()]);
    }
    out
}

#[target_feature(enable = "avx,f16c")]
fn cvtps_ph8(src: &[f32; 8]) -> [u16; 8] {
    let mut dst = [0u16; 8];
    // SAFETY: both arrays hold the eight lanes the instructions read and write.
    unsafe {
        let v = _mm256_loadu_ps(src.as_ptr());
        _mm_storeu_si128(dst.as_mut_ptr().cast::<__m128i>(), _mm256_cvtps_ph::<0>(v));
    }
    dst
}

/// `cvtps2dq` (`_mm_cvtps_epi32`, in the default rounding mode) of each float of `input`,
/// four at a time. SSE2, which every x86-64 CPU has.
pub fn cvtps2dq(input: &[f32]) -> Vec<i32> {
    let mut out = Vec::with_capacity(input.len());
    for block in input.chunks(4) {
        let mut src = [0f32; 4];
        src[..block.len()].copy_from_slice(block);
        let mut dst = [0i32; 4];
        // SAFETY: SSE2 is part of x86-64; both arrays hold the four lanes.
        unsafe {
            let v = _mm_cvtps_epi32(_mm_loadu_ps(src.as_ptr()));
            _mm_storeu_si128(dst.as_mut_ptr().cast::<__m128i>(), v);
        }
        out.extend_from_slice(&dst[..block.len()]);
    }
    out
}

/// `cvttss2si` (`_mm_cvtt_ss2si`) of `f`: SSE, which every x86-64 CPU has.
pub fn cvttss2si(f: f32) -> i32 {
    // SAFETY: SSE is part of x86-64.
    unsafe { _mm_cvtt_ss2si(_mm_set_ss(f)) }
}

/// `vcmpps` with `_CMP_NLT_UQ` and with `_CMP_NGE_UQ` (`_mm256_cmp_ps`) of each pair of `a`
/// and `b`: whether each lane's comparison is true.
pub fn vcmpps_nlt_nge_uq(a: &[f32], b: &[f32]) -> (Vec<bool>, Vec<bool>) {
    need(has_avx(), "AVX");
    assert_eq!(a.len(), b.len());
    let (mut nlt, mut nge) = (Vec::with_capacity(a.len()), Vec::with_capacity(a.len()));
    for (ba, bb) in a.chunks(8).zip(b.chunks(8)) {
        let (mut sa, mut sb) = ([0f32; 8], [0f32; 8]);
        sa[..ba.len()].copy_from_slice(ba);
        sb[..bb.len()].copy_from_slice(bb);
        // SAFETY: the CPU has AVX (checked above).
        let (m1, m2) = unsafe { cmp8(&sa, &sb) };
        for i in 0..ba.len() {
            nlt.push(m1 >> i & 1 != 0);
            nge.push(m2 >> i & 1 != 0);
        }
    }
    (nlt, nge)
}

#[target_feature(enable = "avx")]
fn cmp8(a: &[f32; 8], b: &[f32; 8]) -> (i32, i32) {
    // SAFETY: both arrays hold the eight lanes the loads read.
    let (va, vb): (__m256, __m256) =
        unsafe { (_mm256_loadu_ps(a.as_ptr()), _mm256_loadu_ps(b.as_ptr())) };
    (
        _mm256_movemask_ps(_mm256_cmp_ps::<_CMP_NLT_UQ>(va, vb)),
        _mm256_movemask_ps(_mm256_cmp_ps::<_CMP_NGE_UQ>(va, vb)),
    )
}

/// `psrad` (`_mm_sra_epi32`) of `v` by the 64-bit count `count` in a register (the low
/// quadword of its count operand): SSE2, which every x86-64 CPU has.
pub fn psrad(v: i32, count: u64) -> i32 {
    // SAFETY: SSE2 is part of x86-64; the operands are registers.
    unsafe {
        _mm_cvtsi128_si32(_mm_sra_epi32(
            _mm_cvtsi32_si128(v),
            _mm_cvtsi64_si128(count as i64),
        ))
    }
}

/// `psrlq` (`_mm_srl_epi64`) of `v` by the 64-bit count `count` in a register: SSE2.
pub fn psrlq(v: u64, count: u64) -> u64 {
    // SAFETY: SSE2 is part of x86-64; the operands are registers.
    unsafe {
        _mm_cvtsi128_si64(_mm_srl_epi64(
            _mm_cvtsi64_si128(v as i64),
            _mm_cvtsi64_si128(count as i64),
        )) as u64
    }
}

/// `psllq` (`_mm_sll_epi64`) of `v` by the 64-bit count `count` in a register: SSE2.
pub fn psllq(v: u64, count: u64) -> u64 {
    // SAFETY: SSE2 is part of x86-64; the operands are registers.
    unsafe {
        _mm_cvtsi128_si64(_mm_sll_epi64(
            _mm_cvtsi64_si128(v as i64),
            _mm_cvtsi64_si128(count as i64),
        )) as u64
    }
}

/// x86's `sar r32, cl` of `v` by `cl`.
pub fn sar32(v: i32, cl: u8) -> i32 {
    let mut r = v;
    // SAFETY: a register-only instruction.
    unsafe {
        std::arch::asm!("sar {r:e}, cl", r = inout(reg) r, in("cl") cl, options(pure, nomem, nostack))
    };
    r
}

/// x86's `sar r64, cl` of `v` by `cl`.
pub fn sar64(v: i64, cl: u8) -> i64 {
    let mut r = v;
    // SAFETY: a register-only instruction.
    unsafe {
        std::arch::asm!("sar {r}, cl", r = inout(reg) r, in("cl") cl, options(pure, nomem, nostack))
    };
    r
}

/// x86's `shl r64, cl` of `v` by `cl`.
pub fn shl64(v: u64, cl: u8) -> u64 {
    let mut r = v;
    // SAFETY: a register-only instruction.
    unsafe {
        std::arch::asm!("shl {r}, cl", r = inout(reg) r, in("cl") cl, options(pure, nomem, nostack))
    };
    r
}
