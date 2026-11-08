// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `deflate_decompress_bmi2` (external/deflate/lib/x86/decompress_impl.h:18-40
//! @ libdeflate v1.26): the decompressor's template compiled for BMI2, chosen where the CPU has
//! BMI2, as `arch_select_decompress_func` chooses it (lines 42-55). GCC's build compiles the
//! whole template with `target("bmi2")`, MSVC's uses `_bzhi_u64` for `EXTRACT_VARBITS`; here
//! the template is compiled with BMI2 enabled and the compiler picks the instructions. The
//! results are the portable build's: only the instructions differ.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): the BMI2 build runs
//! only after the CPU reported BMI2.
#![allow(unsafe_code)]

use crate::LibdeflateResult;
use crate::deflate_decompress::Decompressor;

/// The BMI2 build's result, or `None` (having done nothing) when the CPU lacks BMI2.
pub(crate) fn deflate_decompress_bmi2(
    d: &mut Decompressor,
    input: &[u8],
    out: &mut [u8],
    actual_in_nbytes_ret: Option<&mut usize>,
    actual_out_nbytes_ret: Option<&mut usize>,
) -> Option<LibdeflateResult> {
    use std::sync::OnceLock;
    static BMI2: OnceLock<bool> = OnceLock::new();
    if !*BMI2.get_or_init(|| std::arch::is_x86_feature_detected!("bmi2")) {
        return None;
    }
    // SAFETY: the CPU has BMI2.
    Some(unsafe { decompress(d, input, out, actual_in_nbytes_ret, actual_out_nbytes_ret) })
}

#[target_feature(enable = "bmi2")]
fn decompress(
    d: &mut Decompressor,
    input: &[u8],
    out: &mut [u8],
    actual_in_nbytes_ret: Option<&mut usize>,
    actual_out_nbytes_ret: Option<&mut usize>,
) -> LibdeflateResult {
    d.decompress_template(input, out, actual_in_nbytes_ret, actual_out_nbytes_ret)
}
