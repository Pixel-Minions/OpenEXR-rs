// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! The codeblock conversions upstream chooses at `X86_CPU_EXT_LEVEL_AVX2`
//! (`avx2_rev_tx_to_cb32`, `avx2_irv_tx_to_cb32`, `avx2_rev_tx_from_cb32`,
//! `avx2_irv_tx_from_cb32`, `avx2_rev_tx_to_cb64`, `avx2_rev_tx_from_cb64`,
//! external/OpenJPH/src/core/codestream/ojph_codestream_avx2.cpp:84-275 @ OpenJPH 0.32.0;
//! `codeblock_fun::init`, ojph_codeblock_fun.cpp:225-251): the generic conversions
//! (`codestream_gen.rs`; for floats to integers, the rounded one, `cvtps2dq`, as SSE2's and
//! AVX2's), which upstream's lanes compute (the sign set by OR, `-0.0` for a zero magnitude
//! as `-val` gives; the 64-bit magnitude by `cmpgt`, XOR and add, a two's-complement negate),
//! compiled for AVX2. `avx2_find_max_val32` and `avx2_find_max_val64` OR the lanes of a
//! `max_val` the port keeps as one word (the same OR).

#![allow(unsafe_code)]

use crate::codestream_gen as cg;

/// The CPU has AVX2 (upstream's level check implies it).
fn avx2() -> bool {
    std::arch::is_x86_feature_detected!("avx2")
}

macro_rules! avx2_build {
    ($wrapper:ident, $inner:ident, $k:path, ($($p:ident: $t:ty),*)) => {
        /// # Safety
        /// The CPU supports AVX2.
        #[target_feature(enable = "avx2")]
        unsafe fn $inner($($p: $t),*) {
            $k($($p),*);
        }

        /// The conversion's AVX2 build; false where the CPU lacks AVX2.
        pub(crate) fn $wrapper($($p: $t),*) -> bool {
            if !avx2() {
                return false;
            }
            // SAFETY: the CPU has AVX2
            unsafe { $inner($($p),*) };
            true
        }
    };
}

avx2_build!(
    rev_tx_to_cb32,
    rev_tx_to_cb32_avx2,
    cg::rev_tx_to_cb32,
    (sp: &[i32], dp: &mut [u32], k_max: u32, count: u32, max_val: &mut u32)
);
avx2_build!(
    irv_tx_to_cb32_rounded,
    irv_tx_to_cb32_rounded_avx2,
    cg::irv_tx_to_cb32_rounded,
    (sp: &[f32], dp: &mut [u32], delta_inv: f32, count: u32, max_val: &mut u32)
);
avx2_build!(
    rev_tx_from_cb32,
    rev_tx_from_cb32_avx2,
    cg::rev_tx_from_cb32,
    (sp: &[u32], dp: &mut [i32], k_max: u32, count: u32)
);
avx2_build!(
    rev_tx_to_cb64,
    rev_tx_to_cb64_avx2,
    cg::rev_tx_to_cb64,
    (sp: &[i64], dp: &mut [u64], k_max: u32, count: u32, max_val: &mut u64)
);
avx2_build!(
    rev_tx_from_cb64,
    rev_tx_from_cb64_avx2,
    cg::rev_tx_from_cb64,
    (sp: &[u64], dp: &mut [i64], k_max: u32, count: u32)
);
avx2_build!(
    irv_tx_from_cb32,
    irv_tx_from_cb32_avx2,
    cg::irv_tx_from_cb32,
    (sp: &[u32], dp: &mut [f32], delta: f32, count: u32)
);
