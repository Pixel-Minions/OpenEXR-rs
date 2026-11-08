// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! zstd's `DYNAMIC_BMI2` builds (external/zstd/lib/common/portability_macros.h:88-103
//! @ zstd v1.5.7): GCC and clang builds for x86-64 without `-mbmi2` (the Linux wheel) compile
//! the entropy coders' bodies a second time with `BMI2_TARGET_ATTRIBUTE` and take that build
//! where `ZSTD_cpuSupportsBmi2` (BMI1 and BMI2); MSVC builds have only the portable one. The
//! bodies, ported once and always inlined, are compiled here a second time with BMI1 and BMI2
//! enabled and chosen the same way: `FSE_readNCount_body`, `HUF_readStats_body`
//! (common/entropy_common.c:198, 318), `FSE_decompress_wksp_body` (common/fse_decompress.c:
//! 298), the Huffman decoders' `HUF_DGEN` bodies (decompress/huf_decompress.c:108),
//! `ZSTD_buildFSETable_body` and `ZSTD_decompressSequences_body`
//! (decompress/zstd_decompress_block.c:616, 1905), and the encoders' `HUF_compress1X_
//! usingCTable_internal_body` (compress/huf_compress.c:1122) and `ZSTD_encodeSequences_body`
//! (compress/zstd_compress_sequences.c:402). Only the instructions differ.
//!
//! This file is on `cargo xtask guards`' `unsafe` allowlist (PLAN.md D8): each BMI2 build runs
//! only after the CPU reported BMI1 and BMI2.
#![allow(unsafe_code)]

use crate::error_private::ZResult;
use crate::fse_compress::FseCTable;
use crate::huf_compress::HufCTable;
use crate::huf_decompress::HufDTable;
use crate::zstd_compress_internal::SeqDef;
use crate::zstd_decompress_internal::{DCtx, SeqSymbol};

/// Whether this build takes the BMI2 builds: `DYNAMIC_BMI2` (not MSVC) on x86-64, and the
/// CPU has BMI1 and BMI2 (found once).
#[cfg(all(target_arch = "x86_64", not(target_env = "msvc")))]
fn enabled() -> bool {
    use std::sync::OnceLock;
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::arch::is_x86_feature_detected!("bmi1") && std::arch::is_x86_feature_detected!("bmi2")
    })
}

/// `$name`: the BMI2 build of `$body` where [`enabled`], else the portable one.
macro_rules! bmi2_build {
    ($(#[$doc:meta])* $vis:vis fn $name:ident = $($body:ident)::+ ($($a:ident: $t:ty),* $(,)?) -> $r:ty;) => {
        $(#[$doc])*
        #[allow(clippy::too_many_arguments)]
        $vis fn $name($($a: $t),*) -> $r {
            #[cfg(all(target_arch = "x86_64", not(target_env = "msvc")))]
            #[target_feature(enable = "bmi1,bmi2")]
            #[allow(clippy::too_many_arguments)]
            fn built($($a: $t),*) -> $r {
                $($body)::+($($a),*)
            }
            #[cfg(all(target_arch = "x86_64", not(target_env = "msvc")))]
            if enabled() {
                // SAFETY: the CPU has BMI1 and BMI2.
                return unsafe { built($($a),*) };
            }
            $($body)::+($($a),*)
        }
    };
}

bmi2_build!(pub(crate) fn fse_read_ncount = crate::entropy_common::fse_read_ncount_body(
    normalized_counter: &mut [i16], max_sv: &mut u32, table_log: &mut u32, header: &[u8],
) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn huf_read_stats = crate::entropy_common::huf_read_stats_body(
    huff_weight: &mut [u8], rank_stats: &mut [u32], nb_symbols: &mut u32,
    table_log_out: &mut u32, src: &[u8], wksp_size: usize,
) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn fse_decompress_wksp = crate::fse_decompress::fse_decompress_wksp_body(
    dst: &mut [u8], c_src: &[u8], max_log: u32, wksp_size: usize,
) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn decompress_1x1_using_dtable =
    crate::huf_decompress::decompress_1x1_using_dtable_body(
        dst: &mut [u8], c_src: &[u8], dtable: &HufDTable,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn decompress_4x1_using_dtable_body =
    crate::huf_decompress::decompress_4x1_using_dtable_portable(
        dst: &mut [u8], c_src: &[u8], dtable: &HufDTable,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn decompress_1x2_using_dtable =
    crate::huf_decompress::decompress_1x2_using_dtable_body(
        dst: &mut [u8], c_src: &[u8], dtable: &HufDTable,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn decompress_4x2_using_dtable_body =
    crate::huf_decompress::decompress_4x2_using_dtable_portable(
        dst: &mut [u8], c_src: &[u8], dtable: &HufDTable,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn build_fse_table = crate::zstd_decompress_block::build_fse_table_body(
    dt: &mut [SeqSymbol], normalized_counter: &[i16], max_symbol_value: u32,
    base_value: &[u32], nb_additional_bits: &[u8], table_log: u32,
) -> (););

bmi2_build!(pub(crate) fn decompress_sequences =
    crate::zstd_decompress_block::decompress_sequences_body(
        dctx: &mut DCtx, dst: &mut [u8], op_start: usize, max_dst_size: usize,
        seq_src: &[u8], nb_seq: i32,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn zstd_encode_sequences =
    crate::zstd_compress_sequences::zstd_encode_sequences_body(
        dst: &mut [u8], ctable_match_length: &FseCTable, ml_code_table: &[u8],
        ctable_offset_bits: &FseCTable, of_code_table: &[u8], ctable_lit_length: &FseCTable,
        ll_code_table: &[u8], sequences: &[SeqDef], long_offsets: bool,
    ) -> ZResult<usize>;);

bmi2_build!(pub(crate) fn huf_compress1x_using_ctable =
    crate::huf_compress::huf_compress1x_using_ctable_body(
        dst: &mut [u8], src: &[u8], ctable: &HufCTable,
    ) -> usize;);
