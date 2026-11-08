// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! The reversible wavelet kernels upstream chooses at `X86_CPU_EXT_LEVEL_AVX2`
//! (`avx2_rev_vert_step`, `avx2_rev_horz_ana`, `avx2_rev_horz_syn`,
//! external/OpenJPH/src/core/transform/ojph_transform_avx2.cpp:146-1083 @ OpenJPH 0.32.0;
//! `init_wavelet_transform_functions`, ojph_transform.cpp:155-160): the generic kernels'
//! lifting, which upstream's AVX2 loops compute lane by lane (one loop per lifting case, eight
//! 32-bit or four 64-bit samples a step, integer adds, multiplies and arithmetic shifts),
//! compiled for AVX2. The samples inside each line are the same integers; upstream's lanes
//! past a line's width only write padding, which nothing inside the width reads.

#![allow(unsafe_code)]

use crate::mem::LineBuf;
use crate::params::{LiftingStep, ParamAtk};
use crate::transform::{rev_horz_ana_gen, rev_horz_syn_gen, rev_vert_step_gen};

/// The CPU has AVX2 (upstream's level check implies it).
fn avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
unsafe fn rev_vert_step_avx2(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
) {
    rev_vert_step_gen(s, sig, other, aug, repeat, synthesis);
}

/// `avx2_rev_vert_step` (lines 387-407): false where the CPU lacks AVX2.
pub(crate) fn rev_vert_step(
    s: &LiftingStep,
    sig: &LineBuf,
    other: &LineBuf,
    aug: &mut LineBuf,
    repeat: u32,
    synthesis: bool,
) -> bool {
    if !avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2
    unsafe { rev_vert_step_avx2(s, sig, other, aug, repeat, synthesis) };
    true
}

/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
unsafe fn rev_horz_ana_avx2(
    atk: &ParamAtk,
    ldst: &mut LineBuf,
    hdst: &mut LineBuf,
    src: &LineBuf,
    width: u32,
    even: bool,
) {
    rev_horz_ana_gen(atk, ldst, hdst, src, width, even);
}

/// `avx2_rev_horz_ana` (lines 727-745): false where the CPU lacks AVX2.
pub(crate) fn rev_horz_ana(
    atk: &ParamAtk,
    ldst: &mut LineBuf,
    hdst: &mut LineBuf,
    src: &LineBuf,
    width: u32,
    even: bool,
) -> bool {
    if !avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2
    unsafe { rev_horz_ana_avx2(atk, ldst, hdst, src, width, even) };
    true
}

/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
unsafe fn rev_horz_syn_avx2(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: &mut LineBuf,
    hsrc: &mut LineBuf,
    width: u32,
    even: bool,
) {
    rev_horz_syn_gen(atk, dst, lsrc, hsrc, width, even);
}

/// `avx2_rev_horz_syn` (lines 1064-1082): false where the CPU lacks AVX2.
pub(crate) fn rev_horz_syn(
    atk: &ParamAtk,
    dst: &mut LineBuf,
    lsrc: &mut LineBuf,
    hsrc: &mut LineBuf,
    width: u32,
    even: bool,
) -> bool {
    if !avx2() {
        return false;
    }
    // SAFETY: the CPU has AVX2
    unsafe { rev_horz_syn_avx2(atk, dst, lsrc, hsrc, width, even) };
    true
}
