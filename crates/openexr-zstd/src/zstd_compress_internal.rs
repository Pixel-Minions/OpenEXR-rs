// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_compress_internal.h` (external/zstd/lib/compress/zstd_compress_internal.h:1-1636
//! @ zstd v1.5.7) and the compression half of `zstd_internal.h`
//! (external/zstd/lib/common/zstd_internal.h:1-326 @ zstd v1.5.7): the compressor's shared
//! types and helpers, as the ported compressor needs them.

use crate::fse_compress::FseCTable;
use crate::huf_compress::{HUF_CTABLE_SIZE_ST, HufCTable, HufRepeat};
use crate::mem::highbit32;
use crate::zstd_decompress_internal::{
    LL_FSE_LOG, MAX_LL, MAX_ML, MAX_OFF, ML_FSE_LOG, OFF_FSE_LOG,
};

/// `ZSTD_strategy` (external/zstd/lib/zstd.h:336-347 @ zstd v1.5.7), as its values.
pub(crate) type Strategy = u32;
pub(crate) const ZSTD_FAST: Strategy = 1;
pub(crate) const ZSTD_DFAST: Strategy = 2;
pub(crate) const ZSTD_GREEDY: Strategy = 3;
pub(crate) const ZSTD_LAZY: Strategy = 4;
pub(crate) const ZSTD_LAZY2: Strategy = 5;
pub(crate) const ZSTD_BTLAZY2: Strategy = 6;
pub(crate) const ZSTD_BTOPT: Strategy = 7;
pub(crate) const ZSTD_BTULTRA: Strategy = 8;
pub(crate) const ZSTD_BTULTRA2: Strategy = 9;

/// `SymbolEncodingType_e` (external/zstd/lib/common/zstd_internal.h:94 @ zstd v1.5.7).
pub(crate) type SymbolEncodingType = u32;
pub(crate) const SET_BASIC: SymbolEncodingType = 0;
pub(crate) const SET_RLE: SymbolEncodingType = 1;
pub(crate) const SET_COMPRESSED: SymbolEncodingType = 2;
pub(crate) const SET_REPEAT: SymbolEncodingType = 3;

/// `ZSTD_REP_NUM` (external/zstd/lib/common/zstd_internal.h:64 @ zstd v1.5.7).
pub(crate) const ZSTD_REP_NUM: u32 = 3;
/// `MINMATCH` (external/zstd/lib/common/zstd_internal.h:98 @ zstd v1.5.7).
pub(crate) const MINMATCH: u32 = 3;
/// `DefaultMaxOff` (external/zstd/lib/common/zstd_internal.h:105 @ zstd v1.5.7).
pub(crate) const DEFAULT_MAX_OFF: u32 = 28;
/// `MaxFSELog` (external/zstd/lib/common/zstd_internal.h:111 @ zstd v1.5.7).
pub(crate) const MAX_FSE_LOG: u32 = 9;

/// `LL_defaultNorm` (external/zstd/lib/common/zstd_internal.h:126-132 @ zstd v1.5.7).
pub(crate) const LL_DEFAULT_NORM: [i16; MAX_LL as usize + 1] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
/// `LL_defaultNormLog` (external/zstd/lib/common/zstd_internal.h:133-134 @ zstd v1.5.7).
pub(crate) const LL_DEFAULT_NORM_LOG: u32 = 6;
/// `ML_defaultNorm` (external/zstd/lib/common/zstd_internal.h:145-153 @ zstd v1.5.7).
pub(crate) const ML_DEFAULT_NORM: [i16; MAX_ML as usize + 1] = [
    1, 4, 3, 2, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1, -1, -1,
];
/// `ML_defaultNormLog` (external/zstd/lib/common/zstd_internal.h:154-155 @ zstd v1.5.7).
pub(crate) const ML_DEFAULT_NORM_LOG: u32 = 6;
/// `OF_defaultNorm` (external/zstd/lib/common/zstd_internal.h:157-162 @ zstd v1.5.7).
pub(crate) const OF_DEFAULT_NORM: [i16; DEFAULT_MAX_OFF as usize + 1] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];
/// `OF_defaultNormLog` (external/zstd/lib/common/zstd_internal.h:163-164 @ zstd v1.5.7).
pub(crate) const OF_DEFAULT_NORM_LOG: u32 = 5;

/// Port of `ZSTD_hufCTables_t` (external/zstd/lib/compress/zstd_compress_internal.h:63-66
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy)]
pub(crate) struct HufCTables {
    pub(crate) ctable: HufCTable,
    pub(crate) repeat_mode: HufRepeat,
}

impl HufCTables {
    pub(crate) fn new() -> Self {
        HufCTables {
            ctable: [0; HUF_CTABLE_SIZE_ST],
            repeat_mode: HufRepeat::None,
        }
    }
}

/// Port of `FSE_repeat` (external/zstd/lib/common/fse.h:278-282 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FseRepeat {
    /// Cannot use the previous table.
    None,
    /// Can use the previous table, but it must be checked.
    Check,
    /// Can use the previous table, which is assumed valid.
    Valid,
}

/// Port of `ZSTD_fseCTables_t` (external/zstd/lib/compress/zstd_compress_internal.h:68-75
/// @ zstd v1.5.7).
#[derive(Debug, Clone)]
pub(crate) struct FseCTables {
    pub(crate) offcode_ctable: FseCTable,
    pub(crate) matchlength_ctable: FseCTable,
    pub(crate) litlength_ctable: FseCTable,
    pub(crate) offcode_repeat_mode: FseRepeat,
    pub(crate) matchlength_repeat_mode: FseRepeat,
    pub(crate) litlength_repeat_mode: FseRepeat,
}

impl FseCTables {
    pub(crate) fn new() -> Self {
        FseCTables {
            offcode_ctable: FseCTable::new(OFF_FSE_LOG, MAX_OFF),
            matchlength_ctable: FseCTable::new(ML_FSE_LOG, MAX_ML),
            litlength_ctable: FseCTable::new(LL_FSE_LOG, MAX_LL),
            offcode_repeat_mode: FseRepeat::None,
            matchlength_repeat_mode: FseRepeat::None,
            litlength_repeat_mode: FseRepeat::None,
        }
    }

    /// `memcpy (dst, src, sizeof (ZSTD_fseCTables_t))`, the tables' words reused.
    pub(crate) fn copy_from(&mut self, src: &FseCTables) {
        self.offcode_ctable.0.copy_from_slice(&src.offcode_ctable.0);
        self.matchlength_ctable
            .0
            .copy_from_slice(&src.matchlength_ctable.0);
        self.litlength_ctable
            .0
            .copy_from_slice(&src.litlength_ctable.0);
        self.offcode_repeat_mode = src.offcode_repeat_mode;
        self.matchlength_repeat_mode = src.matchlength_repeat_mode;
        self.litlength_repeat_mode = src.litlength_repeat_mode;
    }
}

/// Port of `ZSTD_entropyCTables_t` (external/zstd/lib/compress/zstd_compress_internal.h:77-80
/// @ zstd v1.5.7).
#[derive(Debug, Clone)]
pub(crate) struct EntropyCTables {
    pub(crate) huf: HufCTables,
    pub(crate) fse: FseCTables,
}

impl EntropyCTables {
    pub(crate) fn new() -> Self {
        EntropyCTables {
            huf: HufCTables::new(),
            fse: FseCTables::new(),
        }
    }
}

/// Port of `SeqDef` (external/zstd/lib/compress/zstd_compress_internal.h:85-89 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SeqDef {
    /// `Offset + ZSTD_REP_NUM`, or the repcode 1, 2 or 3.
    pub(crate) off_base: u32,
    pub(crate) lit_length: u16,
    /// `matchLength - MINMATCH`.
    pub(crate) ml_base: u16,
}

/// Port of `ZSTD_LLcode` (external/zstd/lib/compress/zstd_compress_internal.h:584-597
/// @ zstd v1.5.7).
#[inline]
pub(crate) fn ll_code(lit_length: u32) -> u32 {
    const LL_CODE: [u8; 64] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 16, 17, 17, 18, 18, 19, 19, 20,
        20, 20, 20, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23,
        24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    ];
    const LL_DELTA_CODE: u32 = 19;
    if lit_length > 63 {
        highbit32(lit_length) + LL_DELTA_CODE
    } else {
        u32::from(LL_CODE[lit_length as usize])
    }
}

/// Port of `ZSTD_MLcode` (external/zstd/lib/compress/zstd_compress_internal.h:601-614
/// @ zstd v1.5.7): `ml_base` is `matchLength - MINMATCH`.
#[inline]
pub(crate) fn ml_code(ml_base: u32) -> u32 {
    const ML_CODE: [u8; 128] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31, 32, 32, 33, 33, 34, 34, 35, 35, 36, 36, 36, 36, 37, 37, 37, 37,
        38, 38, 38, 38, 38, 38, 38, 38, 39, 39, 39, 39, 39, 39, 39, 39, 40, 40, 40, 40, 40, 40, 40,
        40, 40, 40, 40, 40, 40, 40, 40, 40, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41, 41,
        41, 41, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
        42, 42, 42, 42, 42, 42, 42, 42, 42, 42, 42,
    ];
    const ML_DELTA_CODE: u32 = 36;
    if ml_base > 127 {
        highbit32(ml_base) + ML_DELTA_CODE
    } else {
        u32::from(ML_CODE[ml_base as usize])
    }
}

/// Port of `ZSTD_minGain` (external/zstd/lib/compress/zstd_compress_internal.h:677-683
/// @ zstd v1.5.7): the saving below which a block is stored raw.
#[inline]
pub(crate) fn min_gain(src_size: usize, strat: Strategy) -> usize {
    let minlog = if strat >= ZSTD_BTULTRA { strat - 1 } else { 6 };
    (src_size >> minlog) + 2
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zstd_decompress_internal::{LL_BASE, LL_BITS, ML_BASE, ML_BITS};

    #[test]
    fn length_codes_cover_their_lengths_in_the_decoder_tables() {
        for lit_length in 0..(1u32 << 17) {
            let c = ll_code(lit_length) as usize;
            assert!(
                LL_BASE[c] <= lit_length && lit_length < LL_BASE[c] + (1 << LL_BITS[c]),
                "{lit_length}"
            );
        }
        for ml_base in 0..(1u32 << 17) {
            let c = ml_code(ml_base) as usize;
            let ml = ml_base + MINMATCH;
            assert!(
                ML_BASE[c] <= ml && ml < ML_BASE[c] + (1 << ML_BITS[c]),
                "{ml_base}"
            );
        }
    }
}
