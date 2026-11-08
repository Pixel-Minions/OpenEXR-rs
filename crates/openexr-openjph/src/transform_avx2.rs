// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! The reversible wavelet kernels upstream chooses at `X86_CPU_EXT_LEVEL_AVX2`
//! (`avx2_rev_vert_step`, `avx2_rev_horz_ana`, `avx2_rev_horz_syn`,
//! external/OpenJPH/src/core/transform/ojph_transform_avx2.cpp:146-1083 @ OpenJPH 0.32.0;
//! `init_wavelet_transform_functions`, ojph_transform.cpp:155-160): the generic kernels'
//! lifting, which upstream's AVX2 loops compute lane by lane (one loop per lifting case, eight
//! 32-bit or four 64-bit samples a step, integer adds, multiplies and arithmetic shifts),
//! compiled for AVX2, with the shifts of AVX2's loops by an ATK-Eatk at or past a sample's
//! width (`RevKernel::Avx2`, I-33). The samples inside each line are the same integers;
//! upstream's lanes past a line's width only write padding, which nothing inside the width
//! reads.

#![allow(unsafe_code)]

use crate::mem::LineBuf;
use crate::params::{LiftingStep, ParamAtk};
use crate::transform::{
    IrvSimd, RevKernel, irv_horz_ana_gen, irv_horz_syn_gen, irv_simd, irv_vert_step_gen,
    irv_vert_times_k_gen, rev_horz_ana_gen, rev_horz_syn_gen, rev_vert_step_gen,
};

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
    rev_vert_step_gen(s, sig, other, aug, repeat, synthesis, RevKernel::Avx2);
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
    rev_horz_ana_gen(atk, ldst, hdst, src, width, even, RevKernel::Avx2);
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
    rev_horz_syn_gen(atk, dst, lsrc, hsrc, width, even, RevKernel::Avx2);
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

// The irreversible kernels upstream chooses from `X86_CPU_EXT_LEVEL_AVX` (`avx_irv_*`,
// ojph_transform_avx.cpp) and `X86_CPU_EXT_LEVEL_AVX512` (`avx512_irv_*`,
// ojph_transform_avx512.cpp): the generic kernels' lifting, which their lanes compute (fused
// where GCC builds AVX-512's, `fused_irv_lifting`), compiled for AVX or for AVX-512 (whose
// FMA computes the fused steps in line).

/// The SIMD build to run, where the CPU has its features.
fn irv_build() -> IrvSimd {
    match irv_simd() {
        IrvSimd::Avx512
            if std::arch::is_x86_feature_detected!("avx512f")
                && std::arch::is_x86_feature_detected!("fma") =>
        {
            IrvSimd::Avx512
        }
        IrvSimd::Avx | IrvSimd::Avx512 if std::arch::is_x86_feature_detected!("avx") => {
            IrvSimd::Avx
        }
        _ => IrvSimd::None,
    }
}

macro_rules! irv_builds {
    ($wrapper:ident, $k:ident, $k512:ident, $kavx:ident, ($($p:ident: $t:ty),*)) => {
        /// # Safety
        /// The CPU supports AVX-512F and FMA.
        #[target_feature(enable = "avx512f,fma")]
        unsafe fn $k512($($p: $t),*) {
            $k($($p),*);
        }

        /// # Safety
        /// The CPU supports AVX.
        #[target_feature(enable = "avx")]
        unsafe fn $kavx($($p: $t),*) {
            $k($($p),*);
        }

        /// The kernel's SIMD build upstream's level chooses; false where there is none.
        pub(crate) fn $wrapper($($p: $t),*) -> bool {
            match irv_build() {
                // SAFETY: the CPU has the build's features
                IrvSimd::Avx512 => unsafe { $k512($($p),*) },
                // SAFETY: as above
                IrvSimd::Avx => unsafe { $kavx($($p),*) },
                IrvSimd::None => return false,
            }
            true
        }
    };
}

irv_builds!(
    irv_vert_step,
    irv_vert_step_gen,
    irv_vert_step_avx512,
    irv_vert_step_avx,
    (s: &LiftingStep, sig: &LineBuf, other: &LineBuf, aug: &mut LineBuf, repeat: u32, synthesis: bool)
);
irv_builds!(
    irv_vert_times_k,
    irv_vert_times_k_gen,
    irv_vert_times_k_avx512,
    irv_vert_times_k_avx,
    (k: f32, aug: &mut LineBuf, repeat: u32)
);
irv_builds!(
    irv_horz_ana,
    irv_horz_ana_gen,
    irv_horz_ana_avx512,
    irv_horz_ana_avx,
    (atk: &ParamAtk, ldst: &mut LineBuf, hdst: &mut LineBuf, src: &LineBuf, width: u32, even: bool)
);
irv_builds!(
    irv_horz_syn,
    irv_horz_syn_gen,
    irv_horz_syn_avx512,
    irv_horz_syn_avx,
    (atk: &ParamAtk, dst: &mut LineBuf, lsrc: &mut LineBuf, hsrc: &mut LineBuf, width: u32, even: bool)
);
