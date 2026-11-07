// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of the parts of `zstd_decompress_internal.h`
//! (external/zstd/lib/decompress/zstd_decompress_internal.h:1-246 @ zstd v1.5.7) and
//! `zstd_internal.h` (external/zstd/lib/common/zstd_internal.h:1-324 @ zstd v1.5.7) the
//! decompressor uses: its constants, the sequence decoding tables and the decoding context.
//! The tables are upstream's, extracted from its sources.

use crate::huf_decompress::HufDTable;

/// `ZSTD_REP_NUM`, with `repStartValue` (external/zstd/lib/common/zstd_internal.h:64-65
/// @ zstd v1.5.7).
pub(crate) const REP_START_VALUE: [u32; 3] = [1, 4, 8];
/// `ZSTD_blockHeaderSize` (external/zstd/lib/common/zstd_internal.h:85 @ zstd v1.5.7).
pub(crate) const BLOCK_HEADER_SIZE: usize = 3;
/// `MIN_SEQUENCES_SIZE` (external/zstd/lib/common/zstd_internal.h:90 @ zstd v1.5.7).
pub(crate) const MIN_SEQUENCES_SIZE: usize = 1;
/// `MIN_CBLOCK_SIZE` (external/zstd/lib/common/zstd_internal.h:91 @ zstd v1.5.7).
pub(crate) const MIN_CBLOCK_SIZE: usize = 2;
/// `MIN_LITERALS_FOR_4_STREAMS` (external/zstd/lib/common/zstd_internal.h:92 @ zstd v1.5.7).
pub(crate) const MIN_LITERALS_FOR_4_STREAMS: usize = 6;
/// `LONGNBSEQ` (external/zstd/lib/common/zstd_internal.h:96 @ zstd v1.5.7).
pub(crate) const LONGNBSEQ: i32 = 0x7F00;
/// `MaxML`, `MaxLL`, `MaxOff` (external/zstd/lib/common/zstd_internal.h:103-106 @ zstd v1.5.7).
pub(crate) const MAX_ML: u32 = 52;
pub(crate) const MAX_LL: u32 = 35;
pub(crate) const MAX_OFF: u32 = 31;
/// `MaxSeq` (external/zstd/lib/common/zstd_internal.h:107 @ zstd v1.5.7).
pub(crate) const MAX_SEQ: usize = 52;
/// `MLFSELog`, `LLFSELog`, `OffFSELog` (external/zstd/lib/common/zstd_internal.h:108-110
/// @ zstd v1.5.7).
pub(crate) const ML_FSE_LOG: u32 = 9;
pub(crate) const LL_FSE_LOG: u32 = 9;
pub(crate) const OFF_FSE_LOG: u32 = 8;
/// `ZSTD_BLOCKSIZE_MAX` (external/zstd/lib/zstd.h:147-148 @ zstd v1.5.7).
pub(crate) const BLOCKSIZE_MAX: usize = 1 << 17;
/// `ZSTD_HUFFDTABLE_CAPACITY_LOG` (external/zstd/lib/decompress/zstd_decompress_internal.h:78
/// @ zstd v1.5.7).
pub(crate) const HUFFDTABLE_CAPACITY_LOG: u8 = 12;
/// `HUF_DECOMPRESS_WORKSPACE_SIZE` (external/zstd/lib/common/huf.h:235 @ zstd v1.5.7): the
/// size of the context's workspace for Huffman tables.
pub(crate) const HUF_DECOMPRESS_WORKSPACE_SIZE: usize = (2 << 10) + (1 << 9);

/// Port of `ZSTD_seqSymbol` (external/zstd/lib/decompress/zstd_decompress_internal.h:67-72
/// @ zstd v1.5.7). A table's first cell is its header, `{ fastMode, tableLog }`: the port keeps
/// the table log in the header cell's `base_value`, where upstream's layout puts it.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeqSymbol {
    pub(crate) next_state: u16,
    pub(crate) nb_additional_bits: u8,
    pub(crate) nb_bits: u8,
    pub(crate) base_value: u32,
}

pub(crate) const fn seq(
    next_state: u16,
    nb_additional_bits: u8,
    nb_bits: u8,
    base_value: u32,
) -> SeqSymbol {
    SeqSymbol {
        next_state,
        nb_additional_bits,
        nb_bits,
        base_value,
    }
}

/// `LL_bits` (external/zstd/lib/common/zstd_internal.h:119-125 @ zstd v1.5.7).
pub(crate) const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];

/// `ML_bits` (external/zstd/lib/common/zstd_internal.h:136-144 @ zstd v1.5.7).
pub(crate) const ML_BITS: [u8; 53] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    1, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
];

/// `LL_base` (external/zstd/lib/decompress/zstd_decompress_internal.h:30-35 @ zstd v1.5.7).
pub(crate) const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000, 0x10000,
];

/// `OF_base` (external/zstd/lib/decompress/zstd_decompress_internal.h:37-41 @ zstd v1.5.7).
pub(crate) const OF_BASE: [u32; 32] = [
    0, 1, 1, 5, 0xD, 0x1D, 0x3D, 0x7D, 0xFD, 0x1FD, 0x3FD, 0x7FD, 0xFFD, 0x1FFD, 0x3FFD, 0x7FFD,
    0xFFFD, 0x1FFFD, 0x3FFFD, 0x7FFFD, 0xFFFFD, 0x1FFFFD, 0x3FFFFD, 0x7FFFFD, 0xFFFFFD, 0x1FFFFFD,
    0x3FFFFFD, 0x7FFFFFD, 0xFFFFFFD, 0x1FFFFFFD, 0x3FFFFFFD, 0x7FFFFFFD,
];

/// `OF_bits` (external/zstd/lib/decompress/zstd_decompress_internal.h:43-47 @ zstd v1.5.7).
pub(crate) const OF_BITS: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// `ML_base` (external/zstd/lib/decompress/zstd_decompress_internal.h:49-56 @ zstd v1.5.7).
pub(crate) const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 0x83, 0x103, 0x203,
    0x403, 0x803, 0x1003, 0x2003, 0x4003, 0x8003, 0x10003,
];

/// `LL_defaultDTable` (external/zstd/lib/decompress/zstd_decompress_block.c:364-399 @ zstd v1.5.7):
/// the header cell `{ fastMode, tableLog }` as upstream lays it out, then the cells, each
/// `{ nextState, nbAdditionalBits, nbBits, baseValue }`.
pub(crate) const LL_DEFAULT_DTABLE: [SeqSymbol; 65] = [
    seq(1, 1, 1, 6),
    seq(0, 0, 4, 0),
    seq(16, 0, 4, 0),
    seq(32, 0, 5, 1),
    seq(0, 0, 5, 3),
    seq(0, 0, 5, 4),
    seq(0, 0, 5, 6),
    seq(0, 0, 5, 7),
    seq(0, 0, 5, 9),
    seq(0, 0, 5, 10),
    seq(0, 0, 5, 12),
    seq(0, 0, 6, 14),
    seq(0, 1, 5, 16),
    seq(0, 1, 5, 20),
    seq(0, 1, 5, 22),
    seq(0, 2, 5, 28),
    seq(0, 3, 5, 32),
    seq(0, 4, 5, 48),
    seq(32, 6, 5, 64),
    seq(0, 7, 5, 128),
    seq(0, 8, 6, 256),
    seq(0, 10, 6, 1024),
    seq(0, 12, 6, 4096),
    seq(32, 0, 4, 0),
    seq(0, 0, 4, 1),
    seq(0, 0, 5, 2),
    seq(32, 0, 5, 4),
    seq(0, 0, 5, 5),
    seq(32, 0, 5, 7),
    seq(0, 0, 5, 8),
    seq(32, 0, 5, 10),
    seq(0, 0, 5, 11),
    seq(0, 0, 6, 13),
    seq(32, 1, 5, 16),
    seq(0, 1, 5, 18),
    seq(32, 1, 5, 22),
    seq(0, 2, 5, 24),
    seq(32, 3, 5, 32),
    seq(0, 3, 5, 40),
    seq(0, 6, 4, 64),
    seq(16, 6, 4, 64),
    seq(32, 7, 5, 128),
    seq(0, 9, 6, 512),
    seq(0, 11, 6, 2048),
    seq(48, 0, 4, 0),
    seq(16, 0, 4, 1),
    seq(32, 0, 5, 2),
    seq(32, 0, 5, 3),
    seq(32, 0, 5, 5),
    seq(32, 0, 5, 6),
    seq(32, 0, 5, 8),
    seq(32, 0, 5, 9),
    seq(32, 0, 5, 11),
    seq(32, 0, 5, 12),
    seq(0, 0, 6, 15),
    seq(32, 1, 5, 18),
    seq(32, 1, 5, 20),
    seq(32, 2, 5, 24),
    seq(32, 2, 5, 28),
    seq(32, 3, 5, 40),
    seq(32, 4, 5, 48),
    seq(0, 16, 6, 65536),
    seq(0, 15, 6, 32768),
    seq(0, 14, 6, 16384),
    seq(0, 13, 6, 8192),
];

/// `OF_defaultDTable` (external/zstd/lib/decompress/zstd_decompress_block.c:402-421 @ zstd v1.5.7):
/// the header cell `{ fastMode, tableLog }` as upstream lays it out, then the cells, each
/// `{ nextState, nbAdditionalBits, nbBits, baseValue }`.
pub(crate) const OF_DEFAULT_DTABLE: [SeqSymbol; 33] = [
    seq(1, 1, 1, 5),
    seq(0, 0, 5, 0),
    seq(0, 6, 4, 61),
    seq(0, 9, 5, 509),
    seq(0, 15, 5, 32765),
    seq(0, 21, 5, 2097149),
    seq(0, 3, 5, 5),
    seq(0, 7, 4, 125),
    seq(0, 12, 5, 4093),
    seq(0, 18, 5, 262141),
    seq(0, 23, 5, 8388605),
    seq(0, 5, 5, 29),
    seq(0, 8, 4, 253),
    seq(0, 14, 5, 16381),
    seq(0, 20, 5, 1048573),
    seq(0, 2, 5, 1),
    seq(16, 7, 4, 125),
    seq(0, 11, 5, 2045),
    seq(0, 17, 5, 131069),
    seq(0, 22, 5, 4194301),
    seq(0, 4, 5, 13),
    seq(16, 8, 4, 253),
    seq(0, 13, 5, 8189),
    seq(0, 19, 5, 524285),
    seq(0, 1, 5, 1),
    seq(16, 6, 4, 61),
    seq(0, 10, 5, 1021),
    seq(0, 16, 5, 65533),
    seq(0, 28, 5, 268435453),
    seq(0, 27, 5, 134217725),
    seq(0, 26, 5, 67108861),
    seq(0, 25, 5, 33554429),
    seq(0, 24, 5, 16777213),
];

/// `ML_defaultDTable` (external/zstd/lib/decompress/zstd_decompress_block.c:425-460 @ zstd v1.5.7):
/// the header cell `{ fastMode, tableLog }` as upstream lays it out, then the cells, each
/// `{ nextState, nbAdditionalBits, nbBits, baseValue }`.
pub(crate) const ML_DEFAULT_DTABLE: [SeqSymbol; 65] = [
    seq(1, 1, 1, 6),
    seq(0, 0, 6, 3),
    seq(0, 0, 4, 4),
    seq(32, 0, 5, 5),
    seq(0, 0, 5, 6),
    seq(0, 0, 5, 8),
    seq(0, 0, 5, 9),
    seq(0, 0, 5, 11),
    seq(0, 0, 6, 13),
    seq(0, 0, 6, 16),
    seq(0, 0, 6, 19),
    seq(0, 0, 6, 22),
    seq(0, 0, 6, 25),
    seq(0, 0, 6, 28),
    seq(0, 0, 6, 31),
    seq(0, 0, 6, 34),
    seq(0, 1, 6, 37),
    seq(0, 1, 6, 41),
    seq(0, 2, 6, 47),
    seq(0, 3, 6, 59),
    seq(0, 4, 6, 83),
    seq(0, 7, 6, 131),
    seq(0, 9, 6, 515),
    seq(16, 0, 4, 4),
    seq(0, 0, 4, 5),
    seq(32, 0, 5, 6),
    seq(0, 0, 5, 7),
    seq(32, 0, 5, 9),
    seq(0, 0, 5, 10),
    seq(0, 0, 6, 12),
    seq(0, 0, 6, 15),
    seq(0, 0, 6, 18),
    seq(0, 0, 6, 21),
    seq(0, 0, 6, 24),
    seq(0, 0, 6, 27),
    seq(0, 0, 6, 30),
    seq(0, 0, 6, 33),
    seq(0, 1, 6, 35),
    seq(0, 1, 6, 39),
    seq(0, 2, 6, 43),
    seq(0, 3, 6, 51),
    seq(0, 4, 6, 67),
    seq(0, 5, 6, 99),
    seq(0, 8, 6, 259),
    seq(32, 0, 4, 4),
    seq(48, 0, 4, 4),
    seq(16, 0, 4, 5),
    seq(32, 0, 5, 7),
    seq(32, 0, 5, 8),
    seq(32, 0, 5, 10),
    seq(32, 0, 5, 11),
    seq(0, 0, 6, 14),
    seq(0, 0, 6, 17),
    seq(0, 0, 6, 20),
    seq(0, 0, 6, 23),
    seq(0, 0, 6, 26),
    seq(0, 0, 6, 29),
    seq(0, 0, 6, 32),
    seq(0, 16, 6, 65539),
    seq(0, 15, 6, 32771),
    seq(0, 14, 6, 16387),
    seq(0, 13, 6, 8195),
    seq(0, 12, 6, 4099),
    seq(0, 11, 6, 2051),
    seq(0, 10, 6, 1027),
];

/// Which table a sequence field decodes with (`LLTptr`, `OFTptr`, `MLTptr`): the default one,
/// or the context's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TableSel {
    Default,
    Own,
}

/// Port of `ZSTD_FrameHeader` (external/zstd/lib/zstd.h:1512-1522 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FrameHeader {
    /// if == ZSTD_CONTENTSIZE_UNKNOWN, it means this field is not available. 0 means "empty"
    pub(crate) frame_content_size: u64,
    /// can be very large, up to <= frameContentSize
    pub(crate) window_size: u64,
    pub(crate) block_size_max: u32,
    /// if == ZSTD_skippableFrame, frameContentSize is the size of skippable content
    pub(crate) skippable: bool,
    pub(crate) header_size: u32,
    pub(crate) dict_id: u32,
    pub(crate) checksum_flag: u32,
}

/// The decoding state of `ZSTD_DCtx` (external/zstd/lib/decompress/zstd_decompress_internal.h:136-246
/// @ zstd v1.5.7) that one-shot decompression without a dictionary uses. Output positions
/// are offsets into the frame's output, which starts at 0 (`prefixStart`); there is no
/// earlier segment (`virtualStart == prefixStart`).
#[derive(Debug, Clone)]
pub(crate) struct DCtx {
    pub(crate) ll_ptr: TableSel,
    pub(crate) ml_ptr: TableSel,
    pub(crate) of_ptr: TableSel,
    pub(crate) ll_table: Vec<SeqSymbol>,
    pub(crate) of_table: Vec<SeqSymbol>,
    pub(crate) ml_table: Vec<SeqSymbol>,
    pub(crate) huf_table: HufDTable,
    pub(crate) rep: [u32; 3],
    pub(crate) f_params: FrameHeader,
    pub(crate) lit_entropy: bool,
    pub(crate) fse_entropy: bool,
    pub(crate) is_frame_decompression: bool,
    pub(crate) validate_checksum: bool,
    /// The block's literals (upstream places them in the output, a buffer of its own, or
    /// both, as room allows; their values are the same).
    pub(crate) lit_buffer: Vec<u8>,
    pub(crate) lit_size: usize,
    /// `prefixStart`: where the current frame's output starts.
    pub(crate) prefix_start: usize,
}

impl DCtx {
    /// Port of `ZSTD_initDCtx_internal` and `ZSTD_DCtx_resetParameters`
    /// (external/zstd/lib/decompress/zstd_decompress.c:240-279 @ zstd v1.5.7), for the
    /// defaults Core uses.
    pub(crate) fn new() -> Self {
        DCtx {
            ll_ptr: TableSel::Default,
            ml_ptr: TableSel::Default,
            of_ptr: TableSel::Default,
            ll_table: vec![SeqSymbol::default(); 1 + (1 << LL_FSE_LOG)],
            of_table: vec![SeqSymbol::default(); 1 + (1 << OFF_FSE_LOG)],
            ml_table: vec![SeqSymbol::default(); 1 + (1 << ML_FSE_LOG)],
            huf_table: HufDTable::new(HUFFDTABLE_CAPACITY_LOG),
            rep: REP_START_VALUE,
            f_params: FrameHeader::default(),
            lit_entropy: false,
            fse_entropy: false,
            is_frame_decompression: true,
            validate_checksum: false,
            lit_buffer: Vec::new(),
            lit_size: 0,
            prefix_start: 0,
        }
    }
}
