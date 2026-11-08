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

/// Port of `ZSTD_compressionParameters` (external/zstd/lib/zstd.h:1352-1360 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CParams {
    /// The largest match distance's log.
    pub(crate) window_log: u32,
    /// The multi-probe search table's log.
    pub(crate) chain_log: u32,
    /// The hash table's log.
    pub(crate) hash_log: u32,
    /// The search attempts' log.
    pub(crate) search_log: u32,
    /// The match length searched for.
    pub(crate) min_match: u32,
    /// The match length the search stops at.
    pub(crate) target_length: u32,
    pub(crate) strategy: Strategy,
}

/// `kSearchStrength` (external/zstd/lib/compress/zstd_compress_internal.h:32 @ zstd v1.5.7).
pub(crate) const K_SEARCH_STRENGTH: u32 = 8;
/// `HASH_READ_SIZE` (external/zstd/lib/compress/zstd_compress_internal.h:33 @ zstd v1.5.7).
pub(crate) const HASH_READ_SIZE: u32 = 8;
/// `ZSTD_WINDOW_START_INDEX` (external/zstd/lib/compress/zstd_compress_internal.h:266
/// @ zstd v1.5.7).
pub(crate) const ZSTD_WINDOW_START_INDEX: u32 = 2;
/// `ZSTD_ROW_HASH_CACHE_SIZE` (external/zstd/lib/compress/zstd_compress_internal.h:270
/// @ zstd v1.5.7).
pub(crate) const ZSTD_ROW_HASH_CACHE_SIZE: u32 = 8;

/// Port of `ZSTD_longLengthType_e` (external/zstd/lib/compress/zstd_compress_internal.h:92-96
/// @ zstd v1.5.7): which length of the sequence at `long_length_pos` has 0x10000 more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LongLengthType {
    None,
    LiteralLength,
    MatchLength,
}

/// Port of `SeqStore_t` (external/zstd/lib/compress/zstd_compress_internal.h:98-115
/// @ zstd v1.5.7): a block's sequences, literals and codes.
#[derive(Debug, Clone)]
pub(crate) struct SeqStore {
    pub(crate) sequences: Vec<SeqDef>,
    pub(crate) lit: Vec<u8>,
    pub(crate) ll_code: Vec<u8>,
    pub(crate) ml_code: Vec<u8>,
    pub(crate) of_code: Vec<u8>,
    pub(crate) long_length_type: LongLengthType,
    pub(crate) long_length_pos: u32,
}

impl SeqStore {
    pub(crate) fn new() -> Self {
        SeqStore {
            sequences: Vec::new(),
            lit: Vec::new(),
            ll_code: Vec::new(),
            ml_code: Vec::new(),
            of_code: Vec::new(),
            long_length_type: LongLengthType::None,
            long_length_pos: 0,
        }
    }

    /// Port of `ZSTD_resetSeqStore` (external/zstd/lib/compress/zstd_compress.c:3161-3166
    /// @ zstd v1.5.7).
    pub(crate) fn reset(&mut self) {
        self.lit.clear();
        self.sequences.clear();
        self.long_length_type = LongLengthType::None;
    }

    /// Port of `ZSTD_storeSeqOnly` (external/zstd/lib/compress/zstd_compress_internal.h:734-767
    /// @ zstd v1.5.7) and `ZSTD_storeSeq` (lines 775-811): the literals copied, the lengths
    /// above 0xFFFF marked.
    #[inline]
    pub(crate) fn store_seq(&mut self, literals: &[u8], off_base: u32, match_length: usize) {
        let lit_length = literals.len();
        self.lit.extend_from_slice(literals);
        // literal Length
        if lit_length > 0xFFFF {
            self.long_length_type = LongLengthType::LiteralLength;
            self.long_length_pos = self.sequences.len() as u32;
        }
        // match Length
        let ml_base = match_length - MINMATCH as usize;
        if ml_base > 0xFFFF {
            self.long_length_type = LongLengthType::MatchLength;
            self.long_length_pos = self.sequences.len() as u32;
        }
        self.sequences.push(SeqDef {
            off_base,
            lit_length: lit_length as u16,
            ml_base: ml_base as u16,
        });
    }
}

/// `OFFSET_TO_OFFBASE` (external/zstd/lib/compress/zstd_compress_internal.h:722 @ zstd v1.5.7).
#[inline]
pub(crate) fn offset_to_offbase(o: u32) -> u32 {
    o + ZSTD_REP_NUM
}

/// `OFFBASE_IS_OFFSET` (external/zstd/lib/compress/zstd_compress_internal.h:723 @ zstd v1.5.7).
#[inline]
pub(crate) fn offbase_is_offset(o: u32) -> bool {
    o > ZSTD_REP_NUM
}

/// `REPCODE1_TO_OFFBASE` (external/zstd/lib/compress/zstd_compress_internal.h:718 @ zstd v1.5.7).
pub(crate) const REPCODE1_TO_OFFBASE: u32 = 1;

/// Port of `ZSTD_count` (external/zstd/lib/compress/zstd_compress_internal.h:854-873
/// @ zstd v1.5.7): the bytes `src[p_in..]` and `src[p_match..]` have in common, up to
/// `p_in_limit`. Upstream compares 8, 4, 2 and 1 bytes at a time, to the same count.
#[inline]
pub(crate) fn zstd_count(src: &[u8], p_in: usize, p_match: usize, p_in_limit: usize) -> usize {
    let n = p_in_limit.saturating_sub(p_in);
    let a = &src[p_in..p_in + n];
    let b = &src[p_match..p_match + n];
    let mut k = 0;
    while k + 8 <= n {
        let x = u64::from_le_bytes(a[k..k + 8].try_into().unwrap())
            ^ u64::from_le_bytes(b[k..k + 8].try_into().unwrap());
        if x != 0 {
            return k + (x.trailing_zeros() >> 3) as usize;
        }
        k += 8;
    }
    while k < n && a[k] == b[k] {
        k += 1;
    }
    k
}

/// `MEM_read32` of `src` at `p`.
#[inline]
pub(crate) fn read32(src: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(src[p..p + 4].try_into().unwrap())
}

/// `MEM_readLE64` of `src` at `p`.
#[inline]
pub(crate) fn read64(src: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(src[p..p + 8].try_into().unwrap())
}

const PRIME4BYTES: u32 = 2654435761;
const PRIME5BYTES: u64 = 889523592379;
const PRIME6BYTES: u64 = 227718039650203;
const PRIME7BYTES: u64 = 58295818150454627;
const PRIME8BYTES: u64 = 0xCF1BBCDCB7A56463;

/// Port of `ZSTD_hashPtrSalted` (external/zstd/lib/compress/zstd_compress_internal.h:948-962
/// @ zstd v1.5.7), with `ZSTD_hash4` to `ZSTD_hash8` (lines 903-926): the `h_bits`-bit hash
/// of the `mls` bytes at `src[p..]`, salted; `ZSTD_hashPtr` is the salt 0.
#[inline]
pub(crate) fn hash_ptr_salted(
    src: &[u8],
    p: usize,
    h_bits: u32,
    mls: u32,
    hash_salt: u64,
) -> usize {
    match mls {
        5 => {
            (((read64(src, p) << (64 - 40)).wrapping_mul(PRIME5BYTES) ^ hash_salt) >> (64 - h_bits))
                as usize
        }
        6 => {
            (((read64(src, p) << (64 - 48)).wrapping_mul(PRIME6BYTES) ^ hash_salt) >> (64 - h_bits))
                as usize
        }
        7 => {
            (((read64(src, p) << (64 - 56)).wrapping_mul(PRIME7BYTES) ^ hash_salt) >> (64 - h_bits))
                as usize
        }
        8 => ((read64(src, p).wrapping_mul(PRIME8BYTES) ^ hash_salt) >> (64 - h_bits)) as usize,
        _ => {
            ((read32(src, p).wrapping_mul(PRIME4BYTES) ^ hash_salt as u32) >> (32 - h_bits))
                as usize
        }
    }
}

/// Port of `ZSTD_hashPtr` (external/zstd/lib/compress/zstd_compress_internal.h:930-945
/// @ zstd v1.5.7).
#[inline]
pub(crate) fn hash_ptr(src: &[u8], p: usize, h_bits: u32, mls: u32) -> usize {
    hash_ptr_salted(src, p, h_bits, mls, 0)
}

/// Port of `ZSTD_window_t` (external/zstd/lib/compress/zstd_compress_internal.h:254-264
/// @ zstd v1.5.7), for the context's frames: index `i` is the current input's byte
/// `i - base`. Upstream's `base` is a pointer, `src - index`; here it is the index of the
/// input's first byte, which a frame continuing the last one's indices makes large and an
/// overflow correction can make negative (upstream's pointer then lies before the input).
/// Without dictionaries the window has no extDict (`dictBase` is n/a): each frame's input
/// follows the cleared window of the last one, so `lowLimit == dictLimit`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    /// The index of the input's first byte (`src - base`).
    pub(crate) base: i64,
    /// The index after the input seen (`nextSrc - base`, as wide as upstream's `size_t`).
    pub(crate) next_src: u64,
    /// Below that point, need extDict.
    pub(crate) dict_limit: u32,
    /// Below that point, no more valid data.
    pub(crate) low_limit: u32,
    /// Number of times overflow correction has run since `ZSTD_window_init`.
    pub(crate) nb_overflow_corrections: u32,
}

/// `ZSTD_CURRENT_MAX` on 64-bit (external/zstd/lib/compress/zstd_compress_internal.h:1031
/// @ zstd v1.5.7): above it the indices are corrected.
pub(crate) const ZSTD_CURRENT_MAX: u32 = 3500 << 20;

/// `ZSTD_WINDOW_OVERFLOW_CORRECT_FREQUENTLY` (external/zstd/lib/compress/zstd_compress_internal.h:1084-1090
/// @ zstd v1.5.7): a debug setting, 0 in release builds.
const ZSTD_WINDOW_OVERFLOW_CORRECT_FREQUENTLY: bool = false;

impl Window {
    /// Port of `ZSTD_window_init` (external/zstd/lib/compress/zstd_compress_internal.h:1334-1343
    /// @ zstd v1.5.7): an empty window, whose first position is `ZSTD_WINDOW_START_INDEX`.
    pub(crate) fn init() -> Self {
        Window {
            base: 0,
            next_src: u64::from(ZSTD_WINDOW_START_INDEX),
            dict_limit: ZSTD_WINDOW_START_INDEX,
            low_limit: ZSTD_WINDOW_START_INDEX,
            nb_overflow_corrections: 0,
        }
    }

    /// Port of `ZSTD_window_clear` (external/zstd/lib/compress/zstd_compress_internal.h:1041-1048
    /// @ zstd v1.5.7): the history forgotten, the indices kept.
    pub(crate) fn clear(&mut self) {
        let end = self.next_src as u32;
        self.low_limit = end;
        self.dict_limit = end;
    }

    /// Port of `ZSTD_window_update` (external/zstd/lib/compress/zstd_compress_internal.h:1354-1390
    /// @ zstd v1.5.7) for a frame's input of `src_size` bytes, which never continues the
    /// window's last bytes (its buffer is not the one after them): the input starts a new
    /// segment at the window's end, the old prefix becoming the extDict, which is empty here
    /// (the window was cleared), so too small to keep. The overlap check that follows changes
    /// nothing with an empty extDict. Returns whether the segment was contiguous (never).
    pub(crate) fn update_new_segment(&mut self, src_size: usize) -> bool {
        if src_size == 0 {
            return true;
        }
        // not contiguous
        let distance_from_base = self.next_src;
        self.low_limit = self.dict_limit;
        self.dict_limit = distance_from_base as u32;
        self.base = distance_from_base as i64;
        if self.dict_limit.wrapping_sub(self.low_limit) < HASH_READ_SIZE {
            self.low_limit = self.dict_limit; // too small extDict
        }
        self.next_src = distance_from_base + src_size as u64;
        false
    }

    /// The input's position of index `i`.
    #[inline]
    pub(crate) fn pos(&self, i: u32) -> usize {
        (i64::from(i) - self.base) as usize
    }

    /// The index of the input's position `p`, as upstream's `(U32)(p - base)`.
    #[inline]
    pub(crate) fn idx(&self, p: usize) -> u32 {
        (p as i64 + self.base) as u32
    }

    /// Port of `ZSTD_window_canOverflowCorrect`
    /// (external/zstd/lib/compress/zstd_compress_internal.h:1097-1125 @ zstd v1.5.7), without a
    /// dictionary (`loadedDictEnd` 0): whether correcting at position `src` would lower the
    /// indices enough to be worth it.
    pub(crate) fn can_overflow_correct(&self, cycle_log: u32, max_dist: u32, src: usize) -> bool {
        let cycle_size = 1u32 << cycle_log;
        let curr = self.idx(src);
        let min_index_to_overflow_correct = cycle_size
            .wrapping_add(if max_dist > cycle_size {
                max_dist
            } else {
                cycle_size
            })
            .wrapping_add(ZSTD_WINDOW_START_INDEX);

        // Adjust the min index to backoff the overflow correction frequency, so we don't
        // waste too much CPU in overflow correction. If this computation overflows we don't
        // really care, we just need to make sure it is at least minIndexToOverflowCorrect.
        let adjustment = self.nb_overflow_corrections.wrapping_add(1);
        let scaled = min_index_to_overflow_correct.wrapping_mul(adjustment);
        let adjusted_index = if scaled > min_index_to_overflow_correct {
            scaled
        } else {
            min_index_to_overflow_correct
        };
        let index_large_enough = curr > adjusted_index;

        // Only overflow correct early if the dictionary is invalidated already, so we don't
        // hurt compression ratio.
        let dictionary_invalidated = curr > max_dist;

        index_large_enough && dictionary_invalidated
    }

    /// Port of `ZSTD_window_needOverflowCorrection`
    /// (external/zstd/lib/compress/zstd_compress_internal.h:1132-1146 @ zstd v1.5.7), without a
    /// dictionary: whether the indices up to position `src_end` are getting too large.
    pub(crate) fn need_overflow_correction(
        &self,
        cycle_log: u32,
        max_dist: u32,
        src: usize,
        src_end: usize,
    ) -> bool {
        let curr = self.idx(src_end);
        if ZSTD_WINDOW_OVERFLOW_CORRECT_FREQUENTLY
            && self.can_overflow_correct(cycle_log, max_dist, src)
        {
            return true;
        }
        curr > ZSTD_CURRENT_MAX
    }

    /// Port of `ZSTD_window_correctOverflow`
    /// (external/zstd/lib/compress/zstd_compress_internal.h:1159-1230 @ zstd v1.5.7): lowers the
    /// indices from position `src` on, keeping their low `cycle_log` bits and every index up to
    /// `max_dist` in the past valid; returns the correction, which every stored index takes.
    pub(crate) fn correct_overflow(&mut self, cycle_log: u32, max_dist: u32, src: usize) -> u32 {
        let cycle_size = 1u32 << cycle_log;
        let cycle_mask = cycle_size - 1;
        let curr = self.idx(src);
        let current_cycle = curr & cycle_mask;
        // Ensure newCurrent - maxDist >= ZSTD_WINDOW_START_INDEX.
        let current_cycle_correction = if current_cycle < ZSTD_WINDOW_START_INDEX {
            if cycle_size > ZSTD_WINDOW_START_INDEX {
                cycle_size
            } else {
                ZSTD_WINDOW_START_INDEX
            }
        } else {
            0
        };
        let new_current = current_cycle
            + current_cycle_correction
            + if max_dist > cycle_size {
                max_dist
            } else {
                cycle_size
            };
        let correction = curr - new_current;

        self.base -= i64::from(correction);
        self.next_src -= u64::from(correction);
        if self.low_limit < correction + ZSTD_WINDOW_START_INDEX {
            self.low_limit = ZSTD_WINDOW_START_INDEX;
        } else {
            self.low_limit -= correction;
        }
        if self.dict_limit < correction + ZSTD_WINDOW_START_INDEX {
            self.dict_limit = ZSTD_WINDOW_START_INDEX;
        } else {
            self.dict_limit -= correction;
        }

        self.nb_overflow_corrections = self.nb_overflow_corrections.wrapping_add(1);
        correction
    }

    /// Port of `ZSTD_window_enforceMaxDist`
    /// (external/zstd/lib/compress/zstd_compress_internal.h:1255-1292 @ zstd v1.5.7), without a
    /// dictionary: the window ends `max_dist` before `block_end`.
    pub(crate) fn enforce_max_dist(&mut self, block_end: usize, max_dist: u32) {
        let block_end_idx = self.idx(block_end);
        if block_end_idx > max_dist {
            let new_low_limit = block_end_idx - max_dist;
            if self.low_limit < new_low_limit {
                self.low_limit = new_low_limit;
            }
            if self.dict_limit < self.low_limit {
                self.dict_limit = self.low_limit;
            }
        }
    }
}

/// Port of `ZSTD_updateRep` (external/zstd/lib/compress/zstd_compress_internal.h:817-835
/// @ zstd v1.5.7).
pub(crate) fn update_rep(rep: &mut [u32; 3], off_base: u32, ll0: u32) {
    if offbase_is_offset(off_base) {
        // full offset
        rep[2] = rep[1];
        rep[1] = rep[0];
        rep[0] = off_base - ZSTD_REP_NUM;
    } else {
        // repcode
        let rep_code = off_base - 1 + ll0;
        if rep_code > 0 {
            // note : if repCode==0, no change
            let current_offset = if rep_code == ZSTD_REP_NUM {
                rep[0].wrapping_sub(1)
            } else {
                rep[rep_code as usize]
            };
            rep[2] = if rep_code >= 2 { rep[1] } else { rep[2] };
            rep[1] = rep[0];
            rep[0] = current_offset;
        }
    }
}

/// `ZSTD_OPT_NUM` (external/zstd/lib/common/zstd_internal.h:62 @ zstd v1.5.7).
pub(crate) const ZSTD_OPT_NUM: usize = 1 << 12;
/// `ZSTD_OPT_SIZE` (external/zstd/lib/compress/zstd_compress_internal.h:226 @ zstd v1.5.7).
pub(crate) const ZSTD_OPT_SIZE: usize = ZSTD_OPT_NUM + 3;

/// Port of `ZSTD_match_t` (external/zstd/lib/compress/zstd_compress_internal.h:194-197
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct OptMatch {
    /// Offset sumtype code for the match, using `ZSTD_storeSeq()` format.
    pub(crate) off: u32,
    /// Raw length of match.
    pub(crate) len: u32,
}

/// Port of `ZSTD_optimal_t` (external/zstd/lib/compress/zstd_compress_internal.h:216-222
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Optimal {
    /// Price from beginning of segment to this position.
    pub(crate) price: i32,
    /// Offset of previous match.
    pub(crate) off: u32,
    /// Length of previous match.
    pub(crate) mlen: u32,
    /// Nb of literals since previous match.
    pub(crate) litlen: u32,
    /// Offset history after previous match.
    pub(crate) rep: [u32; 3],
}

/// Port of `ZSTD_OptPrice_e` (external/zstd/lib/compress/zstd_compress_internal.h:224
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OptPrice {
    #[default]
    Dynamic,
    Predef,
}

/// Port of `optState_t` (external/zstd/lib/compress/zstd_compress_internal.h:227-249
/// @ zstd v1.5.7), without a dictionary's symbol costs (n/a) and with literals always
/// compressed (the literal compression mode is never "disable" here).
#[derive(Debug, Clone, Default)]
pub(crate) struct OptState {
    /// Literals statistics, of size 256.
    pub(crate) lit_freq: Vec<u32>,
    /// litLength statistics, of size (MaxLL+1).
    pub(crate) lit_length_freq: Vec<u32>,
    /// matchLength statistics, of size (MaxML+1).
    pub(crate) match_length_freq: Vec<u32>,
    /// offCode statistics, of size (MaxOff+1).
    pub(crate) off_code_freq: Vec<u32>,
    /// The matches found, of size `ZSTD_OPT_SIZE`.
    pub(crate) match_table: Vec<OptMatch>,
    /// All positions tracked by the optimal parser, of size `ZSTD_OPT_SIZE`.
    pub(crate) price_table: Vec<Optimal>,
    pub(crate) lit_sum: u32,
    pub(crate) lit_length_sum: u32,
    pub(crate) match_length_sum: u32,
    pub(crate) off_code_sum: u32,
    pub(crate) lit_sum_base_price: u32,
    pub(crate) lit_length_sum_base_price: u32,
    pub(crate) match_length_sum_base_price: u32,
    pub(crate) off_code_sum_base_price: u32,
    pub(crate) price_type: OptPrice,
}

/// Port of `ZSTD_MatchState_t` (external/zstd/lib/compress/zstd_compress_internal.h:269-315
/// @ zstd v1.5.7), for the match finders without a dictionary.
#[derive(Debug, Clone)]
pub(crate) struct MatchState {
    pub(crate) window: Window,
    /// The index from which to continue the tables' update.
    pub(crate) next_to_update: u32,
    /// The row match finder's hash log, of rows.
    pub(crate) row_hash_log: u32,
    /// The row match finder's tags, each row's head first.
    pub(crate) tag_table: Vec<u8>,
    /// The row match finder's cache of the next positions' hashes.
    pub(crate) hash_cache: [u32; ZSTD_ROW_HASH_CACHE_SIZE as usize],
    /// The row match finder's salt, new at each reset of the context.
    pub(crate) hash_salt: u64,
    /// The hashes the row match finder has searched for, summed, for the next salt.
    pub(crate) hash_salt_entropy: u32,
    pub(crate) hash_table: Vec<u32>,
    pub(crate) chain_table: Vec<u32>,
    pub(crate) cparams: CParams,
    /// The lazy match finders insert only the positions they search.
    pub(crate) lazy_skipping: bool,
    /// The optimal parser's hash table of 3-byte sequences (`minMatch == 3`), and its log.
    pub(crate) hash_table3: Vec<u32>,
    pub(crate) hash_log3: u32,
    /// The optimal parser's state.
    pub(crate) opt: OptState,
}

/// Port of `ZSTD_getLowestMatchIndex`
/// (external/zstd/lib/compress/zstd_compress_internal.h:1395-1407 @ zstd v1.5.7), without a
/// dictionary.
#[inline]
pub(crate) fn get_lowest_match_index(ms: &MatchState, curr: u32, window_log: u32) -> u32 {
    let max_distance = 1u32 << window_log;
    let lowest_valid = ms.window.low_limit;
    if curr - lowest_valid > max_distance {
        curr - max_distance
    } else {
        lowest_valid
    }
}

/// Port of `ZSTD_getLowestPrefixIndex`
/// (external/zstd/lib/compress/zstd_compress_internal.h:1412-1423 @ zstd v1.5.7), without a
/// dictionary.
#[inline]
pub(crate) fn get_lowest_prefix_index(ms: &MatchState, curr: u32, window_log: u32) -> u32 {
    let max_distance = 1u32 << window_log;
    let lowest_valid = ms.window.dict_limit;
    if curr - lowest_valid > max_distance {
        curr - max_distance
    } else {
        lowest_valid
    }
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
