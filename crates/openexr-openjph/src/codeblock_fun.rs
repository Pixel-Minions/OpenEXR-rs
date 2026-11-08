// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_codeblock_fun.cpp`
//! (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.cpp:1-343 @ OpenJPH 0.32.0) and
//! `ojph_codeblock_fun.h` (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.h:1-122
//! @ OpenJPH 0.32.0): a codeblock's kernels, chosen by its transform. Upstream picks SSE2 or
//! AVX2 conversions and the SSSE3 or AVX2 block decoder on x86-64; the conversions compute
//! the generic ones' samples, and the block decoders decode a valid codeblock as the generic
//! one does (their behaviour on a corrupt one is checked with them, docs/deferred.md).

use crate::codestream_gen as cg;

/// Port of `codeblock_fun` (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.h:99-115
/// @ OpenJPH 0.32.0): the kernels of a reversible or an irreversible codeblock. The line
/// is an integer one when reversible, a float one otherwise (the subband's lines and the
/// codeblock's kernels follow the same kernel), so each conversion takes its line's samples.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CodeblockFun {
    reversible: bool,
}

impl CodeblockFun {
    /// Port of `codeblock_fun::init`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.cpp:147-341 @ OpenJPH 0.32.0).
    pub(crate) fn init(reversible: bool) -> Self {
        CodeblockFun { reversible }
    }

    /// `tx_from_cb32` into an integer line: `gen_rev_tx_from_cb32`.
    pub(crate) fn tx_from_cb32_i(&self, sp: &[u32], dp: &mut [i32], k_max: u32, count: u32) {
        debug_assert!(self.reversible);
        cg::rev_tx_from_cb32(sp, dp, k_max, count);
    }

    /// `tx_from_cb32` into a float line: `gen_irv_tx_from_cb32`.
    pub(crate) fn tx_from_cb32_f(&self, sp: &[u32], dp: &mut [f32], delta: f32, count: u32) {
        debug_assert!(!self.reversible);
        cg::irv_tx_from_cb32(sp, dp, delta, count);
    }

    /// `tx_from_cb64` into an integer line: `gen_rev_tx_from_cb64`.
    pub(crate) fn tx_from_cb64_i(&self, sp: &[u64], dp: &mut [i64], k_max: u32, count: u32) {
        debug_assert!(self.reversible);
        cg::rev_tx_from_cb64(sp, dp, k_max, count);
    }

    /// `tx_from_cb64` into a float line: `gen_irv_tx_from_cb64`.
    pub(crate) fn tx_from_cb64_f(&self, sp: &[u64], dp: &mut [f32], delta: f32, count: u32) {
        debug_assert!(!self.reversible);
        cg::irv_tx_from_cb64(sp, dp, delta, count);
    }
}
