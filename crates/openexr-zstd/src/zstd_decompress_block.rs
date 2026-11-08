// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_decompress_block.c` (external/zstd/lib/decompress/zstd_decompress_block.c:1-2209
//! @ zstd v1.5.7): a compressed block's literals and sequences.
//!
//! - The literals are decoded into the context's own buffer. Upstream places them in the
//!   output after the block, at the output's end, or in its own buffer, as room allows; the
//!   literals and the output are the same either way.
//! - The sequences are decoded by `ZSTD_decompressSequences_body`. Upstream switches to a
//!   prefetching decoder (`ZSTD_decompressSequencesLong`) when many offsets are long, which
//!   decodes the same sequences; and its copies (`ZSTD_wildcopy`, `ZSTD_overlapCopy8`) write
//!   up to 32 bytes past a match within the output's room, bytes the next sequence writes
//!   again: the port copies each byte once, which leaves the output the same.

use crate::bitstream::BitDStream;
use crate::entropy_common::fse_read_ncount;
use crate::error_private::{ErrorCode, ZResult};
use crate::huf_decompress::{
    huf_decompress_1x_using_dtable, huf_decompress_1x1_dctx, huf_decompress_4x_huf_only,
    huf_decompress_4x_using_dtable,
};
use crate::mem::{highbit32, read_le16, read_le24, read_le32};
use crate::zstd_decompress_internal::{
    BLOCK_HEADER_SIZE, BLOCKSIZE_MAX, DCtx, HUF_DECOMPRESS_WORKSPACE_SIZE, LL_BASE, LL_BITS,
    LL_DEFAULT_DTABLE, LL_FSE_LOG, LONGNBSEQ, MAX_LL, MAX_ML, MAX_OFF, MAX_SEQ, MIN_CBLOCK_SIZE,
    MIN_LITERALS_FOR_4_STREAMS, MIN_SEQUENCES_SIZE, ML_BASE, ML_BITS, ML_DEFAULT_DTABLE,
    ML_FSE_LOG, OF_BASE, OF_BITS, OF_DEFAULT_DTABLE, OFF_FSE_LOG, SeqSymbol, TableSel,
};

/// `blockType_e` (external/zstd/lib/common/zstd_internal.h:80 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockType {
    Raw = 0,
    Rle = 1,
    Compressed = 2,
    Reserved = 3,
}

/// `blockProperties_t`: a block header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BlockProperties {
    pub(crate) block_type: BlockType,
    pub(crate) last_block: bool,
    pub(crate) orig_size: u32,
}

/// Port of `ZSTD_blockSizeMax` (external/zstd/lib/decompress/zstd_decompress_block.c:47-59
/// @ zstd v1.5.7).
fn block_size_max(dctx: &DCtx) -> usize {
    if dctx.is_frame_decompression {
        dctx.f_params.block_size_max as usize
    } else {
        BLOCKSIZE_MAX
    }
}

/// Port of `ZSTD_getcBlockSize` (external/zstd/lib/decompress/zstd_decompress_block.c:63-77
/// @ zstd v1.5.7): the block header at the front of `src`; returns the block's size in the
/// frame (1 for an RLE block).
pub(crate) fn get_c_block_size(src: &[u8], bp: &mut BlockProperties) -> ZResult<usize> {
    if src.len() < BLOCK_HEADER_SIZE {
        return Err(ErrorCode::SrcSizeWrong);
    }
    let c_block_header = read_le24(src);
    let c_size = c_block_header >> 3;
    bp.last_block = (c_block_header & 1) != 0;
    bp.block_type = match (c_block_header >> 1) & 3 {
        0 => BlockType::Raw,
        1 => BlockType::Rle,
        2 => BlockType::Compressed,
        _ => BlockType::Reserved,
    };
    bp.orig_size = c_size; // only useful for RLE
    if bp.block_type == BlockType::Rle {
        return Ok(1);
    }
    if bp.block_type == BlockType::Reserved {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(c_size as usize)
}

/// Port of `ZSTD_decodeLiteralsBlock` (external/zstd/lib/decompress/zstd_decompress_block.c:134-340
/// @ zstd v1.5.7): the literals section at the front of `src`, into the context's literal
/// buffer; `dst_capacity` is the room the block's output has. Returns the section's size.
fn decode_literals_block(dctx: &mut DCtx, src: &[u8], dst_capacity: usize) -> ZResult<usize> {
    let src_size = src.len();
    if src_size < MIN_CBLOCK_SIZE {
        return Err(ErrorCode::CorruptionDetected);
    }
    let istart = src;
    let lit_enc_type = istart[0] & 3;
    let block_size_max = block_size_max(dctx);
    let expected_write_size = block_size_max.min(dst_capacity);

    match lit_enc_type {
        // set_repeat, set_compressed
        3 | 2 => {
            if lit_enc_type == 3 && !dctx.lit_entropy {
                return Err(ErrorCode::DictionaryCorrupted);
            }
            if src_size < 5 {
                // srcSize >= MIN_CBLOCK_SIZE == 2; here we need up to 5 for case 3
                return Err(ErrorCode::CorruptionDetected);
            }
            let lhl_code = (istart[0] >> 2) & 3;
            let lhc = read_le32(istart) as usize;
            let (single_stream, lh_size, lit_size, lit_c_size) = match lhl_code {
                // 2 - 2 - 10 - 10
                0 | 1 => (lhl_code == 0, 3, (lhc >> 4) & 0x3FF, (lhc >> 14) & 0x3FF),
                // 2 - 2 - 14 - 14
                2 => (false, 4, (lhc >> 4) & 0x3FFF, lhc >> 18),
                // 2 - 2 - 18 - 18
                _ => (
                    false,
                    5,
                    (lhc >> 4) & 0x3FFFF,
                    (lhc >> 22) + (usize::from(istart[4]) << 10),
                ),
            };
            if lit_size > block_size_max {
                return Err(ErrorCode::CorruptionDetected);
            }
            if !single_stream && lit_size < MIN_LITERALS_FOR_4_STREAMS {
                return Err(ErrorCode::LiteralsHeaderWrong);
            }
            if lit_c_size + lh_size > src_size {
                return Err(ErrorCode::CorruptionDetected);
            }
            if expected_write_size < lit_size {
                return Err(ErrorCode::DstSizeTooSmall);
            }

            dctx.lit_buffer.clear();
            dctx.lit_buffer.resize(lit_size, 0);
            let c_src = &istart[lh_size..lh_size + lit_c_size];
            let huf_success = if lit_enc_type == 3 {
                if single_stream {
                    huf_decompress_1x_using_dtable(&mut dctx.lit_buffer, c_src, &dctx.huf_table)
                } else {
                    huf_decompress_4x_using_dtable(&mut dctx.lit_buffer, c_src, &dctx.huf_table)
                }
            } else if single_stream {
                huf_decompress_1x1_dctx(
                    &mut dctx.huf_table,
                    &mut dctx.lit_buffer,
                    c_src,
                    HUF_DECOMPRESS_WORKSPACE_SIZE,
                )
            } else {
                huf_decompress_4x_huf_only(
                    &mut dctx.huf_table,
                    &mut dctx.lit_buffer,
                    c_src,
                    HUF_DECOMPRESS_WORKSPACE_SIZE,
                )
            };
            if huf_success.is_err() {
                return Err(ErrorCode::CorruptionDetected);
            }

            dctx.lit_size = lit_size;
            dctx.lit_entropy = true;
            Ok(lit_c_size + lh_size)
        }

        // set_basic
        0 => {
            let lhl_code = (istart[0] >> 2) & 3;
            let (lh_size, lit_size) = match lhl_code {
                0 | 2 => (1, usize::from(istart[0] >> 3)),
                1 => (2, usize::from(read_le16(istart) >> 4)),
                _ => {
                    if src_size < 3 {
                        return Err(ErrorCode::CorruptionDetected);
                    }
                    (3, (read_le24(istart) >> 4) as usize)
                }
            };
            if lit_size > block_size_max {
                return Err(ErrorCode::CorruptionDetected);
            }
            if expected_write_size < lit_size {
                return Err(ErrorCode::DstSizeTooSmall);
            }
            if lit_size + lh_size > src_size {
                return Err(ErrorCode::CorruptionDetected);
            }
            dctx.lit_buffer.clear();
            dctx.lit_buffer
                .extend_from_slice(&istart[lh_size..lh_size + lit_size]);
            dctx.lit_size = lit_size;
            Ok(lh_size + lit_size)
        }

        // set_rle
        _ => {
            let lhl_code = (istart[0] >> 2) & 3;
            let (lh_size, lit_size) = match lhl_code {
                0 | 2 => (1, usize::from(istart[0] >> 3)),
                1 => {
                    if src_size < 3 {
                        return Err(ErrorCode::CorruptionDetected);
                    }
                    (2, usize::from(read_le16(istart) >> 4))
                }
                _ => {
                    if src_size < 4 {
                        return Err(ErrorCode::CorruptionDetected);
                    }
                    (3, (read_le24(istart) >> 4) as usize)
                }
            };
            if lit_size > block_size_max {
                return Err(ErrorCode::CorruptionDetected);
            }
            if expected_write_size < lit_size {
                return Err(ErrorCode::DstSizeTooSmall);
            }
            dctx.lit_buffer.clear();
            dctx.lit_buffer.resize(lit_size, istart[lh_size]);
            dctx.lit_size = lit_size;
            Ok(lh_size + 1)
        }
    }
}

/// Port of `ZSTD_buildSeqTable_rle` (external/zstd/lib/decompress/zstd_decompress_block.c:463-477
/// @ zstd v1.5.7).
fn build_seq_table_rle(dt: &mut [SeqSymbol], base_value: u32, nb_add_bits: u8) {
    // header: tableLog 0, fastMode 0
    dt[0] = SeqSymbol::default();
    dt[1] = SeqSymbol {
        nb_bits: 0,
        next_state: 0,
        nb_additional_bits: nb_add_bits,
        base_value,
    };
}

/// Port of `ZSTD_buildFSETable_body` (external/zstd/lib/decompress/zstd_decompress_block.c:484-603
/// @ zstd v1.5.7): the decoding table of one sequence field (literal length, match length or
/// offset) from its normalized counts.
pub(crate) fn build_fse_table(
    dt: &mut [SeqSymbol],
    normalized_counter: &[i16],
    max_symbol_value: u32,
    base_value: &[u32],
    nb_additional_bits: &[u8],
    table_log: u32,
) {
    let max_sv1 = max_symbol_value + 1;
    let table_size = 1u32 << table_log;
    let mut symbol_next = [0u16; MAX_SEQ + 1];
    let mut high_threshold = table_size - 1;
    let (header, table_decode) = dt.split_at_mut(1);

    // Init, lay down lowprob symbols
    let mut fast_mode: u32 = 1;
    let large_limit = (1i32 << (table_log - 1)) as i16;
    for s in 0..max_sv1 as usize {
        if normalized_counter[s] == -1 {
            table_decode[high_threshold as usize].base_value = s as u32;
            high_threshold = high_threshold.wrapping_sub(1);
            symbol_next[s] = 1;
        } else {
            if normalized_counter[s] >= large_limit {
                fast_mode = 0;
            }
            symbol_next[s] = normalized_counter[s] as u16;
        }
    }
    // ZSTD_seqSymbol_header { fastMode, tableLog } over the first cell's bytes
    header[0] = SeqSymbol {
        next_state: fast_mode as u16,
        nb_additional_bits: (fast_mode >> 16) as u8,
        nb_bits: (fast_mode >> 24) as u8,
        base_value: table_log,
    };

    // Spread symbols (the two-stage spread of upstream's fast case puts the k-th symbol at
    // (k * step) & mask, as this loop does)
    let table_mask = table_size - 1;
    let step = (table_size >> 1) + (table_size >> 3) + 3;
    let mut position: u32 = 0;
    for s in 0..max_sv1 as usize {
        for _ in 0..normalized_counter[s].max(0) {
            table_decode[position as usize].base_value = s as u32;
            position = (position + step) & table_mask;
            while position > high_threshold {
                position = (position + step) & table_mask; // lowprob area
            }
        }
    }

    // Build Decoding table
    for cell in table_decode.iter_mut().take(table_size as usize) {
        let symbol = cell.base_value as usize;
        let next_state = u32::from(symbol_next[symbol]);
        symbol_next[symbol] = symbol_next[symbol].wrapping_add(1);
        cell.nb_bits = (table_log - highbit32(next_state)) as u8;
        cell.next_state = (next_state << cell.nb_bits).wrapping_sub(table_size) as u16;
        cell.nb_additional_bits = nb_additional_bits[symbol];
        cell.base_value = base_value[symbol];
    }
}

/// The table log in a sequence table's header cell.
#[inline]
fn seq_table_log(dt: &[SeqSymbol]) -> u32 {
    dt[0].base_value
}

/// Port of `ZSTD_buildSeqTable` (external/zstd/lib/decompress/zstd_decompress_block.c:647-693
/// @ zstd v1.5.7): one field's table, of the encoding type the sequence header gives; returns
/// the bytes read.
fn build_seq_table(
    dtable_space: &mut [SeqSymbol],
    dtable_ptr: &mut TableSel,
    type_: u8,
    max: u32,
    max_log: u32,
    src: &[u8],
    base_value: &[u32],
    nb_additional_bits: &[u8],
    flag_repeat_table: bool,
) -> ZResult<usize> {
    match type_ {
        // set_rle
        1 => {
            if src.is_empty() {
                return Err(ErrorCode::SrcSizeWrong);
            }
            if u32::from(src[0]) > max {
                return Err(ErrorCode::CorruptionDetected);
            }
            let symbol = usize::from(src[0]);
            build_seq_table_rle(dtable_space, base_value[symbol], nb_additional_bits[symbol]);
            *dtable_ptr = TableSel::Own;
            Ok(1)
        }
        // set_basic
        0 => {
            *dtable_ptr = TableSel::Default;
            Ok(0)
        }
        // set_repeat
        3 => {
            if !flag_repeat_table {
                return Err(ErrorCode::CorruptionDetected);
            }
            Ok(0)
        }
        // set_compressed
        _ => {
            let mut table_log = 0u32;
            let mut max = max;
            let mut norm = [0i16; MAX_SEQ + 1];
            let header_size = fse_read_ncount(&mut norm, &mut max, &mut table_log, src)
                .map_err(|_| ErrorCode::CorruptionDetected)?;
            if table_log > max_log {
                return Err(ErrorCode::CorruptionDetected);
            }
            build_fse_table(
                dtable_space,
                &norm,
                max,
                base_value,
                nb_additional_bits,
                table_log,
            );
            *dtable_ptr = TableSel::Own;
            Ok(header_size)
        }
    }
}

/// Port of `ZSTD_decodeSeqHeaders` (external/zstd/lib/decompress/zstd_decompress_block.c:695-775
/// @ zstd v1.5.7): the number of sequences and the three fields' tables; returns the header's
/// size.
fn decode_seq_headers(dctx: &mut DCtx, nb_seq_out: &mut i32, src: &[u8]) -> ZResult<usize> {
    let iend = src.len();
    let mut ip = 0usize;

    // check
    if src.len() < MIN_SEQUENCES_SIZE {
        return Err(ErrorCode::SrcSizeWrong);
    }

    // SeqHead
    let mut nb_seq = i32::from(src[ip]);
    ip += 1;
    if nb_seq > 0x7F {
        if nb_seq == 0xFF {
            if ip + 2 > iend {
                return Err(ErrorCode::SrcSizeWrong);
            }
            nb_seq = i32::from(read_le16(&src[ip..])) + LONGNBSEQ;
            ip += 2;
        } else {
            if ip >= iend {
                return Err(ErrorCode::SrcSizeWrong);
            }
            nb_seq = ((nb_seq - 0x80) << 8) + i32::from(src[ip]);
            ip += 1;
        }
    }
    *nb_seq_out = nb_seq;

    if nb_seq == 0 {
        // No sequence : section ends immediately
        if ip != iend {
            return Err(ErrorCode::CorruptionDetected); // extraneous data present
        }
        return Ok(ip);
    }

    // FSE table descriptors
    if ip + 1 > iend {
        return Err(ErrorCode::SrcSizeWrong); // minimum possible size: 1 byte for symbol encoding types
    }
    if src[ip] & 3 != 0 {
        return Err(ErrorCode::CorruptionDetected); // The last field, Reserved, must be all-zeroes.
    }
    let ll_type = src[ip] >> 6;
    let of_type = (src[ip] >> 4) & 3;
    let ml_type = (src[ip] >> 2) & 3;
    ip += 1;

    // Build DTables
    let fse = dctx.fse_entropy;
    let ll_h_size = build_seq_table(
        &mut dctx.ll_table,
        &mut dctx.ll_ptr,
        ll_type,
        MAX_LL,
        LL_FSE_LOG,
        &src[ip..],
        &LL_BASE,
        &LL_BITS,
        fse,
    )
    .map_err(|_| ErrorCode::CorruptionDetected)?;
    ip += ll_h_size;

    let of_h_size = build_seq_table(
        &mut dctx.of_table,
        &mut dctx.of_ptr,
        of_type,
        MAX_OFF,
        OFF_FSE_LOG,
        &src[ip..],
        &OF_BASE,
        &OF_BITS,
        fse,
    )
    .map_err(|_| ErrorCode::CorruptionDetected)?;
    ip += of_h_size;

    let ml_h_size = build_seq_table(
        &mut dctx.ml_table,
        &mut dctx.ml_ptr,
        ml_type,
        MAX_ML,
        ML_FSE_LOG,
        &src[ip..],
        &ML_BASE,
        &ML_BITS,
        fse,
    )
    .map_err(|_| ErrorCode::CorruptionDetected)?;
    ip += ml_h_size;

    Ok(ip)
}

/// `seq_t`.
#[derive(Debug, Clone, Copy)]
struct Seq {
    lit_length: usize,
    match_length: usize,
    offset: usize,
}

/// `ZSTD_fseState`: a state and its table.
struct FseState<'t> {
    state: usize,
    table: &'t [SeqSymbol],
}

impl<'t> FseState<'t> {
    /// Port of `ZSTD_initFseState` (external/zstd/lib/decompress/zstd_decompress_block.c:1191-1201
    /// @ zstd v1.5.7).
    fn init(bit_d: &mut BitDStream<'_>, dt: &'t [SeqSymbol]) -> Self {
        let state = bit_d.read_bits(seq_table_log(dt)) as usize;
        bit_d.reload();
        FseState {
            state,
            table: &dt[1..],
        }
    }

    /// Port of `ZSTD_updateFseStateWithDInfo`
    /// (external/zstd/lib/decompress/zstd_decompress_block.c:1203-1208 @ zstd v1.5.7).
    #[inline]
    fn update(&mut self, bit_d: &mut BitDStream<'_>, next_state: u16, nb_bits: u32) {
        let low_bits = bit_d.read_bits(nb_bits) as usize;
        self.state = usize::from(next_state).wrapping_add(low_bits);
    }

    #[inline]
    fn cell(&self) -> SeqSymbol {
        self.table[self.state]
    }
}

/// Port of `ZSTD_decodeSequence` (external/zstd/lib/decompress/zstd_decompress_block.c:1228-1346
/// @ zstd v1.5.7), as 64-bit hosts build it.
fn decode_sequence(
    d: &mut BitDStream<'_>,
    ll: &mut FseState<'_>,
    ml: &mut FseState<'_>,
    of: &mut FseState<'_>,
    prev_offset: &mut [usize; 3],
    is_last_seq: bool,
) -> Seq {
    let ll_d_info = ll.cell();
    let ml_d_info = ml.cell();
    let of_d_info = of.cell();
    let mut seq = Seq {
        match_length: ml_d_info.base_value as usize,
        lit_length: ll_d_info.base_value as usize,
        offset: 0,
    };
    let of_base = of_d_info.base_value as usize;
    let ll_bits = u32::from(ll_d_info.nb_additional_bits);
    let ml_bits = u32::from(ml_d_info.nb_additional_bits);
    let of_bits = u32::from(of_d_info.nb_additional_bits);
    let total_bits = ll_bits + ml_bits + of_bits;

    // sequence
    let offset: usize;
    if of_bits > 1 {
        // <= (ZSTD_WINDOWLOG_MAX-1) bits
        offset = of_base.wrapping_add(d.read_bits_fast(of_bits) as usize);
        prev_offset[2] = prev_offset[1];
        prev_offset[1] = prev_offset[0];
        prev_offset[0] = offset;
    } else {
        let ll0 = usize::from(ll_d_info.base_value == 0);
        if of_bits == 0 {
            offset = prev_offset[ll0];
            prev_offset[1] = prev_offset[usize::from(ll0 == 0)];
            prev_offset[0] = offset;
        } else {
            let o = of_base + ll0 + d.read_bits_fast(1) as usize;
            let mut temp = if o == 3 {
                prev_offset[0].wrapping_sub(1)
            } else {
                prev_offset[o]
            };
            // 0 is not valid: input corrupted => force offset to -1 => corruption detected
            // at execSequence
            temp = temp.wrapping_sub(usize::from(temp == 0));
            if o != 1 {
                prev_offset[2] = prev_offset[1];
            }
            prev_offset[1] = prev_offset[0];
            prev_offset[0] = temp;
            offset = temp;
        }
    }
    seq.offset = offset;

    if ml_bits > 0 {
        seq.match_length += d.read_bits_fast(ml_bits) as usize;
    }

    // STREAM_ACCUMULATOR_MIN_64 (57) - (LLFSELog + MLFSELog + OffFSELog)
    if total_bits >= 57 - (LL_FSE_LOG + ML_FSE_LOG + OFF_FSE_LOG) {
        d.reload();
    }

    // Ensure there are enough bits to read the rest of data in 64-bit mode.
    if ll_bits > 0 {
        seq.lit_length += d.read_bits_fast(ll_bits) as usize;
    }

    if !is_last_seq {
        // don't update FSE state for last Sequence
        ll.update(d, ll_d_info.next_state, u32::from(ll_d_info.nb_bits)); // <=  9 bits
        ml.update(d, ml_d_info.next_state, u32::from(ml_d_info.nb_bits)); // <=  9 bits
        of.update(d, of_d_info.next_state, u32::from(of_d_info.nb_bits)); // <=  8 bits
        d.reload();
    }

    seq
}

/// Port of `ZSTD_execSequence` (external/zstd/lib/decompress/zstd_decompress_block.c:999-1096
/// @ zstd v1.5.7), with `ZSTD_execSequenceEnd` (lines 905-948):
/// the sequence's literals from `*lit_ptr`, then its match, at `op` of `dst[..oend]`; returns
/// the bytes written. Offsets reach back at most to the frame's start (no dictionary).
fn exec_sequence(
    dst: &mut [u8],
    prefix_start: usize,
    op: usize,
    oend: usize,
    sequence: Seq,
    lit: &[u8],
    lit_ptr: &mut usize,
) -> ZResult<usize> {
    let sequence_length = sequence.lit_length.wrapping_add(sequence.match_length);
    let lit_limit = lit.len();

    // the checks of ZSTD_execSequenceEnd, taken when the sequence does not fit with room
    // for the wild copies (upstream's fast path needs none: the sequence fits)
    let o_match_end = op.checked_add(sequence_length);
    let i_lit_end = lit_ptr.checked_add(sequence.lit_length);
    let fits = matches!((i_lit_end, o_match_end), (Some(i), Some(o)) if i <= lit_limit && o <= oend.saturating_sub(32));
    if !fits {
        if sequence_length > oend - op {
            return Err(ErrorCode::DstSizeTooSmall); // last match must fit within dstBuffer
        }
        if sequence.lit_length > lit_limit - *lit_ptr {
            return Err(ErrorCode::CorruptionDetected); // try to read beyond literal buffer
        }
    }

    // copy Literals
    let o_lit_end = op + sequence.lit_length;
    dst[op..o_lit_end].copy_from_slice(&lit[*lit_ptr..*lit_ptr + sequence.lit_length]);
    *lit_ptr += sequence.lit_length;

    // copy Match
    if sequence.offset > o_lit_end - prefix_start {
        // offset beyond prefix -> go into extDict, which does not exist here
        // (virtualStart == prefixStart)
        return Err(ErrorCode::CorruptionDetected);
    }
    // An LZ77 copy, byte after byte in effect: upstream's ZSTD_overlapCopy8 and wild copies
    // (lines 1060-1094) give the bytes of a forward byte-by-byte copy. Here in spans that do
    // not overlap their source: after `w` bytes, `dst[m..o_lit_end + w]` repeats with period
    // `offset` (a multiple of which `o_lit_end + w - m` is), so the next `offset + w` bytes
    // can come from `m`; a match not longer than its offset is one copy.
    let m = o_lit_end - sequence.offset;
    let len = sequence.match_length;
    // (an offset of 0 copies each byte onto itself, as the byte loop did: nothing changes)
    let mut w = if sequence.offset == 0 { len } else { 0 };
    while w < len {
        let n = (len - w).min(sequence.offset + w);
        dst.copy_within(m..m + n, o_lit_end + w);
        w += n;
    }
    Ok(sequence_length)
}

/// Port of `ZSTD_decompressSequences_body` (external/zstd/lib/decompress/zstd_decompress_block.c:1613-1690
/// @ zstd v1.5.7): the block's sequences executed into `dst[op_start..op_start + max_dst_size]`;
/// returns the bytes written.
fn decompress_sequences(
    dctx: &mut DCtx,
    dst: &mut [u8],
    op_start: usize,
    max_dst_size: usize,
    seq_src: &[u8],
    mut nb_seq: i32,
) -> ZResult<usize> {
    let oend = op_start + max_dst_size;
    let mut op = op_start;
    let lit = std::mem::take(&mut dctx.lit_buffer);
    let lit_end = dctx.lit_size;
    let mut lit_ptr = 0usize;
    let prefix_start = dctx.prefix_start;

    let result = (|| -> ZResult<usize> {
        // Regen sequences
        if nb_seq != 0 {
            dctx.fse_entropy = true;
            let mut prev_offset = dctx.rep.map(|r| r as usize);
            let (mut d, _) =
                BitDStream::init(seq_src).map_err(|_| ErrorCode::CorruptionDetected)?;
            let ll_t: &[SeqSymbol] = match dctx.ll_ptr {
                TableSel::Default => &LL_DEFAULT_DTABLE,
                TableSel::Own => &dctx.ll_table,
            };
            let of_t: &[SeqSymbol] = match dctx.of_ptr {
                TableSel::Default => &OF_DEFAULT_DTABLE,
                TableSel::Own => &dctx.of_table,
            };
            let ml_t: &[SeqSymbol] = match dctx.ml_ptr {
                TableSel::Default => &ML_DEFAULT_DTABLE,
                TableSel::Own => &dctx.ml_table,
            };
            let mut state_ll = FseState::init(&mut d, ll_t);
            let mut state_offb = FseState::init(&mut d, of_t);
            let mut state_ml = FseState::init(&mut d, ml_t);

            while nb_seq != 0 {
                let sequence = decode_sequence(
                    &mut d,
                    &mut state_ll,
                    &mut state_ml,
                    &mut state_offb,
                    &mut prev_offset,
                    nb_seq == 1,
                );
                let one_seq_size = exec_sequence(
                    dst,
                    prefix_start,
                    op,
                    oend,
                    sequence,
                    &lit[..lit_end],
                    &mut lit_ptr,
                )?;
                op += one_seq_size;
                nb_seq -= 1;
            }

            // check if reached exact end
            if !d.end_of_stream() {
                return Err(ErrorCode::CorruptionDetected);
            }
            // save reps for next block
            for i in 0..3 {
                dctx.rep[i] = prev_offset[i] as u32;
            }
        }

        // last literal segment
        let last_ll_size = lit_end - lit_ptr;
        if last_ll_size > oend - op {
            return Err(ErrorCode::DstSizeTooSmall);
        }
        dst[op..op + last_ll_size].copy_from_slice(&lit[lit_ptr..lit_end]);
        op += last_ll_size;
        Ok(op - op_start)
    })();
    dctx.lit_buffer = lit;
    result
}

/// Port of `ZSTD_decompressBlock_internal` (external/zstd/lib/decompress/zstd_decompress_block.c:2065-2174
/// @ zstd v1.5.7): a compressed block into `dst[op..op + dst_capacity]`; returns its size.
pub(crate) fn decompress_block_internal(
    dctx: &mut DCtx,
    dst: &mut [u8],
    op: usize,
    dst_capacity: usize,
    src: &[u8],
) -> ZResult<usize> {
    // blockType == blockCompressed
    if src.len() > block_size_max(dctx) {
        return Err(ErrorCode::SrcSizeWrong);
    }

    // Decode literals section
    let lit_c_size = decode_literals_block(dctx, src, dst_capacity)?;
    let ip = &src[lit_c_size..];

    // Build Decoding Tables
    let mut nb_seq = 0i32;
    let seq_h_size = decode_seq_headers(dctx, &mut nb_seq, ip)?;
    let ip = &ip[seq_h_size..];

    if dst_capacity == 0 && nb_seq > 0 {
        return Err(ErrorCode::DstSizeTooSmall);
    }

    decompress_sequences(dctx, dst, op, dst_capacity, ip, nb_seq)
}
