// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `huf_decompress.c` (external/zstd/lib/decompress/huf_decompress.c:1-1944
//! @ zstd v1.5.7): the Huffman decoder of zstd's literals, with single-symbol (X1) and
//! double-symbol (X2) tables, one or four streams.
//!
//! The portable decoders (`*_usingDTable_internal_body`) and, for four streams, the fast loops
//! upstream tries first (`*_usingDTable_internal_fast`, in C, or on Linux in x86-64 assembly
//! that translates the C), chosen as upstream's build chooses them ([`fast_decode`]). The two
//! decode the same symbols from a valid stream, but not the same corrupt ones: the fast loops
//! check only that each stream fills its segment, not that its bits end exactly, so they
//! accept streams the portable body refuses (PLAN.md O3: each path ported as upstream takes
//! it). A write past the output, which only a corrupt stream can make, is refused as
//! corruption (upstream's would write past its buffer).

use crate::bitstream::{BitDStream, BitDStreamStatus};
pub(crate) use crate::bmi2::{
    decompress_1x1_using_dtable, decompress_1x2_using_dtable, decompress_4x1_using_dtable_body,
    decompress_4x2_using_dtable_body,
};
use crate::entropy_common::{HUF_READ_STATS_WORKSPACE_SIZE, HUF_TABLELOG_MAX, huf_read_stats};
use crate::error_private::{ErrorCode, ZResult};
use crate::mem::read_le16;

/// `HUF_DECODER_FAST_TABLELOG` (external/zstd/lib/decompress/huf_decompress.c:31
/// @ zstd v1.5.7).
const HUF_DECODER_FAST_TABLELOG: u32 = 11;
/// `HUF_SYMBOLVALUE_MAX` (external/zstd/lib/common/huf.h:39 @ zstd v1.5.7).
const HUF_SYMBOLVALUE_MAX: usize = 255;
/// `HUF_TABLELOG_ABSOLUTEMAX` (external/zstd/lib/common/huf.h:41 @ zstd v1.5.7).
const HUF_TABLELOG_ABSOLUTEMAX: usize = 12;

/// `sizeof (HUF_ReadDTableX1_Workspace)`
/// (external/zstd/lib/decompress/huf_decompress.c:377-383 @ zstd v1.5.7).
const READ_DTABLE_X1_WKSP: usize =
    4 * (2 * (HUF_TABLELOG_ABSOLUTEMAX + 1)) + HUF_READ_STATS_WORKSPACE_SIZE + 2 * 256;
/// `sizeof (HUF_ReadDTableX2_Workspace)`
/// (external/zstd/lib/decompress/huf_decompress.c:1170-1177 @ zstd v1.5.7).
const READ_DTABLE_X2_WKSP: usize = 4 * (12 * 13)
    + 4 * (HUF_TABLELOG_MAX as usize + 1)
    + 4 * (HUF_TABLELOG_MAX as usize + 3)
    + 2 * 256
    + HUF_READ_STATS_WORKSPACE_SIZE;

/// `HUF_DEltX1`: a single-symbol cell.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DEltX1 {
    nb_bits: u8,
    byte: u8,
}

/// `HUF_DEltX2`: a double-symbol cell.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DEltX2 {
    sequence: u16,
    nb_bits: u8,
    length: u8,
}

/// A `HUF_DTable`: its `DTableDesc` and its cells, of the table type in use.
#[derive(Debug, Clone)]
pub(crate) struct HufDTable {
    pub(crate) max_table_log: u8,
    pub(crate) table_type: u8,
    pub(crate) table_log: u8,
    x1: Vec<DEltX1>,
    x2: Vec<DEltX2>,
}

impl HufDTable {
    /// A table of capacity `max_table_log`, as zstd's decoder initializes its own:
    /// `(HUF_DTable) (maxTableLog * 0x1000001)`.
    pub(crate) fn new(max_table_log: u8) -> Self {
        HufDTable {
            max_table_log,
            table_type: 0,
            table_log: 0,
            x1: vec![DEltX1::default(); 2 << max_table_log],
            x2: vec![DEltX2::default(); 1 << max_table_log],
        }
    }
}

/// Port of `HUF_rescaleStats` (external/zstd/lib/decompress/huf_decompress.c:352-375
/// @ zstd v1.5.7): raises the table log to `target_table_log`, rescaling the weights.
fn huf_rescale_stats(
    huff_weight: &mut [u8],
    rank_val: &mut [u32],
    nb_symbols: u32,
    table_log: u32,
    target_table_log: u32,
) -> u32 {
    if table_log > target_table_log {
        return table_log;
    }
    if table_log < target_table_log {
        let scale = target_table_log - table_log;
        for w in &mut huff_weight[..nb_symbols as usize] {
            *w += if *w == 0 { 0 } else { scale as u8 };
        }
        // Update rankVal to reflect the new weights. All weights except 0 get moved to
        // weight + scale. Weights [1, scale] are empty.
        let mut s = target_table_log;
        while s > scale {
            rank_val[s as usize] = rank_val[(s - scale) as usize];
            s -= 1;
        }
        for s in (1..=scale).rev() {
            rank_val[s as usize] = 0;
        }
    }
    target_table_log
}

/// Port of `HUF_readDTableX1_wksp` (external/zstd/lib/decompress/huf_decompress.c:385-519
/// @ zstd v1.5.7): a single-symbol table from the tree at the front of `src`; returns the
/// tree's size.
pub(crate) fn huf_read_dtable_x1(
    dtable: &mut HufDTable,
    src: &[u8],
    wksp_size: usize,
) -> ZResult<usize> {
    let mut table_log = 0u32;
    let mut nb_symbols = 0u32;
    let mut rank_val = [0u32; HUF_TABLELOG_ABSOLUTEMAX + 1];
    let mut rank_start = [0u32; HUF_TABLELOG_ABSOLUTEMAX + 1];
    let mut symbols = [0u8; HUF_SYMBOLVALUE_MAX + 1];
    let mut huff_weight = [0u8; HUF_SYMBOLVALUE_MAX + 1];

    if READ_DTABLE_X1_WKSP > wksp_size {
        return Err(ErrorCode::TableLogTooLarge);
    }

    let i_size = huf_read_stats(
        &mut huff_weight,
        &mut rank_val,
        &mut nb_symbols,
        &mut table_log,
        src,
        HUF_READ_STATS_WORKSPACE_SIZE,
    )?;

    // Table header
    let max_table_log = u32::from(dtable.max_table_log) + 1;
    let target_table_log = max_table_log.min(HUF_DECODER_FAST_TABLELOG);
    table_log = huf_rescale_stats(
        &mut huff_weight,
        &mut rank_val,
        nb_symbols,
        table_log,
        target_table_log,
    );
    if table_log > u32::from(dtable.max_table_log) + 1 {
        // DTable too small, Huffman tree cannot fit in
        return Err(ErrorCode::TableLogTooLarge);
    }
    dtable.table_type = 0;
    dtable.table_log = table_log as u8;

    // Compute symbols and rankStart given rankVal: symbols ordered by weight, rankStart the
    // offset where each rank belongs in the DTable.
    let mut next_rank_start = 0u32;
    for n in 0..=table_log as usize {
        let curr = next_rank_start;
        next_rank_start += rank_val[n];
        rank_start[n] = curr;
    }
    for n in 0..nb_symbols as usize {
        let w = usize::from(huff_weight[n]);
        symbols[rank_start[w] as usize] = n as u8;
        rank_start[w] += 1;
    }

    // fill DTable: all entries of each weight in order.
    let dt = &mut dtable.x1;
    let mut symbol = rank_val[0] as usize;
    let mut rank_start_at = 0usize;
    for w in 1..=table_log {
        let symbol_count = rank_val[w as usize] as usize;
        let length = (1usize << w) >> 1;
        let mut u_start = rank_start_at;
        let nb_bits = (table_log + 1 - w) as u8;
        for s in 0..symbol_count {
            let d = DEltX1 {
                byte: symbols[symbol + s],
                nb_bits,
            };
            dt[u_start..u_start + length].fill(d);
            u_start += length;
        }
        symbol += symbol_count;
        rank_start_at += symbol_count * length;
    }
    Ok(i_size)
}

/// An output pointer of the decoders: a position in the whole output, which a corrupt stream
/// may push past it.
#[inline]
fn put1(dst: &mut [u8], p: usize, v: u8) -> ZResult<()> {
    *dst.get_mut(p).ok_or(ErrorCode::CorruptionDetected)? = v;
    Ok(())
}

/// Port of `HUF_decodeSymbolX1` (external/zstd/lib/decompress/huf_decompress.c:521-528
/// @ zstd v1.5.7).
#[inline]
fn decode_symbol_x1(d: &mut BitDStream<'_>, dt: &[DEltX1], dt_log: u32) -> u8 {
    let val = d.look_bits_fast(dt_log) as usize; // note : dtLog >= 1
    let c = dt[val].byte;
    d.skip_bits(u32::from(dt[val].nb_bits));
    c
}

/// Port of `HUF_decodeStreamX1` (external/zstd/lib/decompress/huf_decompress.c:545-572
/// @ zstd v1.5.7): symbols into `dst[p..p_end]`.
fn decode_stream_x1(
    dst: &mut [u8],
    mut p: usize,
    d: &mut BitDStream<'_>,
    p_end: usize,
    dt: &[DEltX1],
    dt_log: u32,
) -> ZResult<()> {
    // up to 4 symbols at a time
    if p_end as isize - p as isize > 3 {
        loop {
            let unfinished = d.reload() == BitDStreamStatus::Unfinished;
            if !(unfinished & (p < p_end - 3)) {
                break;
            }
            for _ in 0..4 {
                put1(dst, p, decode_symbol_x1(d, dt, dt_log))?;
                p += 1;
            }
        }
    } else {
        d.reload();
    }

    // [0-3] symbols remaining (64-bit: no 32-bit loop)
    while p < p_end {
        put1(dst, p, decode_symbol_x1(d, dt, dt_log))?;
        p += 1;
    }
    Ok(())
}

/// Port of `HUF_decompress1X1_usingDTable_internal_body`
/// (external/zstd/lib/decompress/huf_decompress.c:574-596 @ zstd v1.5.7).
#[inline(always)]
pub(crate) fn decompress_1x1_using_dtable_body(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    let (mut bit_d, _) = BitDStream::init(c_src)?;
    let dst_size = dst.len();
    decode_stream_x1(
        dst,
        0,
        &mut bit_d,
        dst_size,
        &dtable.x1,
        u32::from(dtable.table_log),
    )?;
    if !bit_d.end_of_stream() {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(dst_size)
}

/// The four streams of a 4-stream block: their bit streams, and the starts of the output
/// segments 2 to 4. Port of the common setup of `HUF_decompress4X1_usingDTable_internal_body`
/// and `HUF_decompress4X2_usingDTable_internal_body`.
fn four_streams<'a>(
    c_src: &'a [u8],
    dst_size: usize,
) -> ZResult<([BitDStream<'a>; 4], [usize; 4])> {
    let c_src_size = c_src.len();
    if c_src_size < 10 {
        // strict minimum : jump table + 1 byte per stream
        return Err(ErrorCode::CorruptionDetected);
    }
    if dst_size < 6 {
        // stream 4-split doesn't work
        return Err(ErrorCode::CorruptionDetected);
    }
    let length1 = usize::from(read_le16(c_src));
    let length2 = usize::from(read_le16(&c_src[2..]));
    let length3 = usize::from(read_le16(&c_src[4..]));
    let length4 = c_src_size.wrapping_sub(length1 + length2 + length3 + 6);
    let istart1 = 6;
    let istart2 = istart1 + length1;
    let istart3 = istart2 + length2;
    let istart4 = istart3 + length3;
    let segment_size = dst_size.div_ceil(4);
    let op_start2 = segment_size;
    let op_start3 = op_start2 + segment_size;
    let op_start4 = op_start3 + segment_size;

    if length4 > c_src_size {
        return Err(ErrorCode::CorruptionDetected); // overflow
    }
    if op_start4 > dst_size {
        return Err(ErrorCode::CorruptionDetected); // overflow
    }
    let (b1, _) = BitDStream::init(&c_src[istart1..istart2])?;
    let (b2, _) = BitDStream::init(&c_src[istart2..istart3])?;
    let (b3, _) = BitDStream::init(&c_src[istart3..istart4])?;
    let (b4, _) = BitDStream::init(&c_src[istart4..istart4 + length4])?;
    Ok(([b1, b2, b3, b4], [0, op_start2, op_start3, op_start4]))
}

/// Port of `HUF_decompress4X1_usingDTable_internal_body`
/// (external/zstd/lib/decompress/huf_decompress.c:598-698 @ zstd v1.5.7).
#[inline(always)]
pub(crate) fn decompress_4x1_using_dtable_portable(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    let dst_size = dst.len();
    let (mut bits, starts) = four_streams(c_src, dst_size)?;
    let oend = dst_size;
    let olimit = oend as isize - 3;
    let dt = &dtable.x1;
    let dt_log = u32::from(dtable.table_log);
    let mut op = starts;
    let mut end_signal = true;

    // up to 16 symbols per loop (4 symbols per stream) in 64-bit mode
    if oend - op[3] >= 8 {
        while end_signal & ((op[3] as isize) < olimit) {
            for _ in 0..4 {
                for s in 0..4 {
                    put1(dst, op[s], decode_symbol_x1(&mut bits[s], dt, dt_log))?;
                    op[s] += 1;
                }
            }
            for b in bits.iter_mut() {
                end_signal &= b.reload_fast() == BitDStreamStatus::Unfinished;
            }
        }
    }

    // check corruption (note : should not be necessary : op# advance in lock step, and we
    // control op4)
    if op[0] > starts[1] || op[1] > starts[2] || op[2] > starts[3] {
        return Err(ErrorCode::CorruptionDetected);
    }
    // note : op4 supposed already verified within main loop

    // finish bitStreams one by one
    let ends = [starts[1], starts[2], starts[3], oend];
    for s in 0..4 {
        decode_stream_x1(dst, op[s], &mut bits[s], ends[s], dt, dt_log)?;
    }

    // check
    if !(bits.iter().all(BitDStream::end_of_stream)) {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(dst_size)
}

/// Port of `HUF_buildDEltX2U32` and `HUF_buildDEltX2`
/// (external/zstd/lib/decompress/huf_decompress.c:961-990 @ zstd v1.5.7), little-endian.
fn build_delt_x2(symbol: u32, nb_bits: u32, base_seq: u32, level: u32) -> DEltX2 {
    let seq = if level == 1 {
        symbol
    } else {
        base_seq + (symbol << 8)
    };
    DEltX2 {
        sequence: seq as u16,
        nb_bits: nb_bits as u8,
        length: level as u8,
    }
}

/// Port of `HUF_fillDTableX2ForWeight` (external/zstd/lib/decompress/huf_decompress.c:1010-1065
/// @ zstd v1.5.7): each symbol of `sorted` gets `1 << (tableLog - nbBits)` cells from `at`.
fn fill_dtable_x2_for_weight(
    dt: &mut [DEltX2],
    mut at: usize,
    sorted: &[u8],
    nb_bits: u32,
    table_log: u32,
    base_seq: u16,
    level: u32,
) {
    let length = 1usize << (table_log.wrapping_sub(nb_bits) & 0x1F);
    for &sym in sorted {
        let d = build_delt_x2(u32::from(sym), nb_bits, u32::from(base_seq), level);
        dt[at..at + length].fill(d);
        at += length;
    }
}

/// Port of `HUF_fillDTableX2Level2` (external/zstd/lib/decompress/huf_decompress.c:1069-1122
/// @ zstd v1.5.7).
fn fill_dtable_x2_level2(
    dt: &mut [DEltX2],
    at: usize,
    target_log: u32,
    consumed_bits: u32,
    rank_val: &[u32; 13],
    min_weight: i32,
    max_weight1: i32,
    sorted_symbols: &[u8],
    rank_start: &[u32],
    nb_bits_baseline: u32,
    base_seq: u16,
) {
    // Fill skipped values (all positions up to rankVal[minWeight]). These are positions
    // only get a single symbol because the combined weight is too large.
    if min_weight > 1 {
        let length = 1usize << (target_log.wrapping_sub(consumed_bits) & 0x1F);
        let d = build_delt_x2(u32::from(base_seq), consumed_bits, 0, 1);
        let skip_size = rank_val[min_weight as usize] as usize;
        // upstream lays down 2 cells per 8-byte write: 2 cells for a length of 2, 4 for 4,
        // and groups of 8 up to the skip size otherwise; the cells past the skip size are
        // written again below
        let n = match length {
            2 => 2,
            4 => 4,
            _ => skip_size.div_ceil(8) * 8,
        };
        dt[at..at + n].fill(d);
    }

    // Fill each of the second level symbols by weight.
    for w in min_weight..max_weight1 {
        let begin = rank_start[w as usize] as usize;
        let end = rank_start[w as usize + 1] as usize;
        let nb_bits = nb_bits_baseline - w as u32;
        let total_bits = nb_bits + consumed_bits;
        fill_dtable_x2_for_weight(
            dt,
            at + rank_val[w as usize] as usize,
            &sorted_symbols[begin..end],
            total_bits,
            target_log,
            base_seq,
            2,
        );
    }
}

/// Port of `HUF_fillDTableX2` (external/zstd/lib/decompress/huf_decompress.c:1124-1168
/// @ zstd v1.5.7). `rank_start` is upstream's `rankStart0`: weight `w`'s symbols are
/// `sorted_list[rank_start[w]..rank_start[w + 1]]`.
fn fill_dtable_x2(
    dt: &mut [DEltX2],
    target_log: u32,
    sorted_list: &[u8],
    rank_start: &[u32],
    rank_val_origin: &[[u32; 13]; 12],
    max_weight: u32,
    nb_bits_baseline: u32,
) {
    let rank_val = &rank_val_origin[0];
    // note : targetLog >= srcLog, hence scaleLog <= 1
    let scale_log = nb_bits_baseline as i32 - target_log as i32;
    let min_bits = nb_bits_baseline - max_weight;
    let w_end = max_weight as i32 + 1;

    // Fill DTable in order of weight.
    for w in 1..w_end {
        let begin = rank_start[w as usize] as usize;
        let end = rank_start[w as usize + 1] as usize;
        let nb_bits = nb_bits_baseline - w as u32;

        if target_log.wrapping_sub(nb_bits) >= min_bits {
            // Enough room for a second symbol.
            let mut start = rank_val[w as usize] as usize;
            let length = 1usize << (target_log.wrapping_sub(nb_bits) & 0x1F);
            let mut min_weight = nb_bits as i32 + scale_log;
            if min_weight < 1 {
                min_weight = 1;
            }
            // Fill the DTable for every symbol of weight w. These symbols get at least 1
            // second symbol.
            for &sym in &sorted_list[begin..end] {
                fill_dtable_x2_level2(
                    dt,
                    start,
                    target_log,
                    nb_bits,
                    &rank_val_origin[nb_bits as usize],
                    min_weight,
                    w_end,
                    sorted_list,
                    rank_start,
                    nb_bits_baseline,
                    u16::from(sym),
                );
                start += length;
            }
        } else {
            // Only a single symbol.
            fill_dtable_x2_for_weight(
                dt,
                rank_val[w as usize] as usize,
                &sorted_list[begin..end],
                nb_bits,
                target_log,
                0,
                1,
            );
        }
    }
}

/// Port of `HUF_readDTableX2_wksp` (external/zstd/lib/decompress/huf_decompress.c:1179-1262
/// @ zstd v1.5.7): a double-symbol table from the tree at the front of `src`; returns the
/// tree's size.
pub(crate) fn huf_read_dtable_x2(
    dtable: &mut HufDTable,
    src: &[u8],
    wksp_size: usize,
) -> ZResult<usize> {
    let mut table_log = 0u32;
    let mut nb_symbols = 0u32;
    let mut max_table_log = u32::from(dtable.max_table_log);
    let mut rank_val = [[0u32; 13]; 12];
    let mut rank_stats = [0u32; HUF_TABLELOG_MAX as usize + 1];
    let mut rank_start0 = [0u32; HUF_TABLELOG_MAX as usize + 3];
    let mut sorted_symbol = [0u8; HUF_SYMBOLVALUE_MAX + 1];
    let mut weight_list = [0u8; HUF_SYMBOLVALUE_MAX + 1];

    if READ_DTABLE_X2_WKSP > wksp_size {
        return Err(ErrorCode::Generic);
    }
    if max_table_log > HUF_TABLELOG_MAX {
        return Err(ErrorCode::TableLogTooLarge);
    }

    let i_size = huf_read_stats(
        &mut weight_list,
        &mut rank_stats,
        &mut nb_symbols,
        &mut table_log,
        src,
        HUF_READ_STATS_WORKSPACE_SIZE,
    )?;

    // check result
    if table_log > max_table_log {
        // DTable can't fit code depth
        return Err(ErrorCode::TableLogTooLarge);
    }
    if table_log <= HUF_DECODER_FAST_TABLELOG && max_table_log > HUF_DECODER_FAST_TABLELOG {
        max_table_log = HUF_DECODER_FAST_TABLELOG;
    }

    // find maxWeight (necessarily finds a solution before 0)
    let mut max_w = table_log;
    while rank_stats[max_w as usize] == 0 {
        max_w -= 1;
    }

    // Get start index of each weight (rankStart is rankStart0 + 1)
    {
        let rank_start = &mut rank_start0[1..];
        let mut next_rank_start = 0u32;
        for w in 1..=max_w as usize {
            let curr = next_rank_start;
            next_rank_start += rank_stats[w];
            rank_start[w] = curr;
        }
        rank_start[0] = next_rank_start; // put all 0w symbols at the end of sorted list
        rank_start[max_w as usize + 1] = next_rank_start;

        // sort symbols by weight
        for s in 0..nb_symbols as usize {
            let w = usize::from(weight_list[s]);
            let r = rank_start[w] as usize;
            rank_start[w] += 1;
            sorted_symbol[r] = s as u8;
        }
        rank_start[0] = 0; // forget 0w symbols; this is beginning of weight(1)
    }

    // Build rankVal
    {
        let rescale = (max_table_log as i32 - table_log as i32) - 1; // tableLog <= maxTableLog
        let mut next_rank_val = 0u32;
        for w in 1..=max_w as usize {
            let curr = next_rank_val;
            next_rank_val += rank_stats[w] << (w as i32 + rescale);
            rank_val[0][w] = curr;
        }
        let min_bits = table_log + 1 - max_w;
        let mut consumed = min_bits;
        while consumed < max_table_log - min_bits + 1 {
            for w in 1..=max_w as usize {
                rank_val[consumed as usize][w] = rank_val[0][w] >> consumed;
            }
            consumed += 1;
        }
    }

    fill_dtable_x2(
        &mut dtable.x2,
        max_table_log,
        &sorted_symbol,
        &rank_start0,
        &rank_val,
        max_w,
        table_log + 1,
    );

    dtable.table_log = max_table_log as u8;
    dtable.table_type = 1;
    Ok(i_size)
}

/// `memcpy (op, &dt[val].sequence, n)`: the first `n` bytes of a cell's sequence.
#[inline]
fn put_seq(dst: &mut [u8], p: usize, seq: u16, n: usize) -> ZResult<()> {
    let b = seq.to_le_bytes();
    dst.get_mut(p..p + n)
        .ok_or(ErrorCode::CorruptionDetected)?
        .copy_from_slice(&b[..n]);
    Ok(())
}

/// Port of `HUF_decodeSymbolX2` (external/zstd/lib/decompress/huf_decompress.c:1265-1272
/// @ zstd v1.5.7): writes two bytes, returns how many were decoded.
#[inline]
fn decode_symbol_x2(
    dst: &mut [u8],
    p: usize,
    d: &mut BitDStream<'_>,
    dt: &[DEltX2],
    dt_log: u32,
) -> ZResult<usize> {
    let val = d.look_bits_fast(dt_log) as usize; // note : dtLog >= 1
    put_seq(dst, p, dt[val].sequence, 2)?;
    d.skip_bits(u32::from(dt[val].nb_bits));
    Ok(usize::from(dt[val].length))
}

/// Port of `HUF_decodeLastSymbolX2` (external/zstd/lib/decompress/huf_decompress.c:1274-1290
/// @ zstd v1.5.7).
#[inline]
fn decode_last_symbol_x2(
    dst: &mut [u8],
    p: usize,
    d: &mut BitDStream<'_>,
    dt: &[DEltX2],
    dt_log: u32,
) -> ZResult<usize> {
    let val = d.look_bits_fast(dt_log) as usize; // note : dtLog >= 1
    put_seq(dst, p, dt[val].sequence, 1)?;
    if dt[val].length == 1 {
        d.skip_bits(u32::from(dt[val].nb_bits));
    } else if d.bits_consumed < 64 {
        d.skip_bits(u32::from(dt[val].nb_bits));
        if d.bits_consumed > 64 {
            // ugly hack; works only because it's the last symbol. Note : can't easily
            // extract nbBits from just this symbol
            d.bits_consumed = 64;
        }
    }
    Ok(1)
}

/// Port of `HUF_decodeStreamX2` (external/zstd/lib/decompress/huf_decompress.c:1307-1350
/// @ zstd v1.5.7): symbols into `dst[p..p_end]`.
fn decode_stream_x2(
    dst: &mut [u8],
    mut p: usize,
    d: &mut BitDStream<'_>,
    p_end: usize,
    dt: &[DEltX2],
    dt_log: u32,
) -> ZResult<()> {
    let left = p_end as isize - p as isize;
    // up to 8 symbols at a time
    if left >= 8 {
        if dt_log <= 11 {
            // up to 10 symbols at a time
            loop {
                let unfinished = d.reload() == BitDStreamStatus::Unfinished;
                if !(unfinished & ((p as isize) < p_end as isize - 9)) {
                    break;
                }
                for _ in 0..5 {
                    p += decode_symbol_x2(dst, p, d, dt, dt_log)?;
                }
            }
        } else {
            // up to 8 symbols at a time
            loop {
                let unfinished = d.reload() == BitDStreamStatus::Unfinished;
                if !(unfinished & ((p as isize) < p_end as isize - 7)) {
                    break;
                }
                for _ in 0..4 {
                    p += decode_symbol_x2(dst, p, d, dt, dt_log)?;
                }
            }
        }
    } else {
        d.reload();
    }

    // closer to end : up to 2 symbols at a time
    if p_end as isize - p as isize >= 2 {
        loop {
            let unfinished = d.reload() == BitDStreamStatus::Unfinished;
            if !(unfinished & ((p as isize) <= p_end as isize - 2)) {
                break;
            }
            p += decode_symbol_x2(dst, p, d, dt, dt_log)?;
        }
        while (p as isize) <= p_end as isize - 2 {
            // no need to reload : reached the end of DStream
            p += decode_symbol_x2(dst, p, d, dt, dt_log)?;
        }
    }

    if p < p_end {
        decode_last_symbol_x2(dst, p, d, dt, dt_log)?;
    }
    Ok(())
}

/// Port of `HUF_decompress1X2_usingDTable_internal_body`
/// (external/zstd/lib/decompress/huf_decompress.c:1352-1377 @ zstd v1.5.7).
#[inline(always)]
pub(crate) fn decompress_1x2_using_dtable_body(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    let (mut bit_d, _) = BitDStream::init(c_src)?;
    let dst_size = dst.len();
    decode_stream_x2(
        dst,
        0,
        &mut bit_d,
        dst_size,
        &dtable.x2,
        u32::from(dtable.table_log),
    )?;
    if !bit_d.end_of_stream() {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(dst_size)
}

/// Port of `HUF_decompress4X2_usingDTable_internal_body`
/// (external/zstd/lib/decompress/huf_decompress.c:1383-1501 @ zstd v1.5.7), as GCC and MSVC
/// build it.
#[inline(always)]
pub(crate) fn decompress_4x2_using_dtable_portable(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    let dst_size = dst.len();
    let (mut bits, starts) = four_streams(c_src, dst_size)?;
    let oend = dst_size;
    let olimit = oend as isize - 7;
    let dt = &dtable.x2;
    let dt_log = u32::from(dtable.table_log);
    let mut op = starts;
    let mut end_signal = true;

    // 16-32 symbols per loop (4-8 symbols per stream)
    if oend - op[3] >= 8 {
        while end_signal & ((op[3] as isize) < olimit) {
            for _ in 0..4 {
                for s in 0..4 {
                    op[s] += decode_symbol_x2(dst, op[s], &mut bits[s], dt, dt_log)?;
                }
            }
            let mut all = true;
            for b in bits.iter_mut() {
                all &= b.reload_fast() == BitDStreamStatus::Unfinished;
            }
            end_signal = all;
        }
    }

    // check corruption
    if op[0] > starts[1] || op[1] > starts[2] || op[2] > starts[3] {
        return Err(ErrorCode::CorruptionDetected);
    }
    // note : op4 already verified within main loop

    // finish bitStreams one by one
    let ends = [starts[1], starts[2], starts[3], oend];
    for s in 0..4 {
        decode_stream_x2(dst, op[s], &mut bits[s], ends[s], dt, dt_log)?;
    }

    // check
    if !(bits.iter().all(BitDStream::end_of_stream)) {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(dst_size)
}

/// Port of `algoTime` (external/zstd/lib/decompress/huf_decompress.c:1794-1813 @ zstd v1.5.7):
/// `{ tableTime, decode256Time }` for single and double symbols, by compression ratio.
const ALGO_TIME: [[(u32, u32); 2]; 16] = [
    [(0, 0), (1, 1)], // Q==0 : impossible
    [(0, 0), (1, 1)], // Q==1 : impossible
    [(150, 216), (381, 119)],
    [(170, 205), (514, 112)],
    [(177, 199), (539, 110)],
    [(197, 194), (644, 107)],
    [(221, 192), (735, 107)],
    [(256, 189), (881, 106)],
    [(359, 188), (1167, 109)],
    [(582, 187), (1570, 114)],
    [(688, 187), (1712, 122)],
    [(825, 186), (1965, 136)],
    [(976, 185), (2131, 150)],
    [(1180, 186), (2070, 175)],
    [(1377, 185), (1731, 202)],
    [(1412, 185), (1695, 202)],
];

/// Port of `HUF_selectDecoder` (external/zstd/lib/decompress/huf_decompress.c:1820-1843
/// @ zstd v1.5.7): 0 for the single-symbol decoder, 1 for the double-symbol one.
fn huf_select_decoder(dst_size: usize, c_src_size: usize) -> u32 {
    // decoder timing evaluation
    let q = if c_src_size >= dst_size {
        15
    } else {
        c_src_size * 16 / dst_size
    }; // Q < 16
    let d256 = (dst_size >> 8) as u32;
    let d_time0 = ALGO_TIME[q][0].0 + ALGO_TIME[q][0].1 * d256;
    let mut d_time1 = ALGO_TIME[q][1].0 + ALGO_TIME[q][1].1 * d256;
    d_time1 += d_time1 >> 5; // small advantage to algorithm using less memory
    u32::from(d_time1 < d_time0)
}

/// Port of `HUF_decompress1X_usingDTable` (external/zstd/lib/decompress/huf_decompress.c:1876-1891
/// @ zstd v1.5.7): one stream, with the table already read.
pub(crate) fn huf_decompress_1x_using_dtable(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    if dtable.table_type != 0 {
        decompress_1x2_using_dtable(dst, c_src, dtable)
    } else {
        decompress_1x1_using_dtable(dst, c_src, dtable)
    }
}

/// Port of `HUF_decompress4X_usingDTable` (external/zstd/lib/decompress/huf_decompress.c:1907-1922
/// @ zstd v1.5.7): four streams, with the table already read.
pub(crate) fn huf_decompress_4x_using_dtable(
    dst: &mut [u8],
    c_src: &[u8],
    dtable: &HufDTable,
) -> ZResult<usize> {
    if dtable.table_type != 0 {
        decompress_4x2_using_dtable(dst, c_src, dtable)
    } else {
        decompress_4x1_using_dtable(dst, c_src, dtable)
    }
}

/// Port of `HUF_decompress1X1_DCtx_wksp` (external/zstd/lib/decompress/huf_decompress.c:1894-1904
/// @ zstd v1.5.7): a single-symbol tree, then one stream.
pub(crate) fn huf_decompress_1x1_dctx(
    dtable: &mut HufDTable,
    dst: &mut [u8],
    c_src: &[u8],
    wksp_size: usize,
) -> ZResult<usize> {
    let h_size = huf_read_dtable_x1(dtable, c_src, wksp_size)?;
    if h_size >= c_src.len() {
        return Err(ErrorCode::SrcSizeWrong);
    }
    decompress_1x1_using_dtable(dst, &c_src[h_size..], dtable)
}

/// Port of `HUF_decompress4X_hufOnly_wksp` (external/zstd/lib/decompress/huf_decompress.c:1924-1944
/// @ zstd v1.5.7), with `HUF_decompress4X1_DCtx_wksp` (lines 930-942) and
/// `HUF_decompress4X2_DCtx_wksp` (lines 1769-1782): a tree of the faster kind for these sizes,
/// then four streams.
pub(crate) fn huf_decompress_4x_huf_only(
    dtable: &mut HufDTable,
    dst: &mut [u8],
    c_src: &[u8],
    wksp_size: usize,
) -> ZResult<usize> {
    // validation checks
    if dst.is_empty() {
        return Err(ErrorCode::DstSizeTooSmall);
    }
    if c_src.is_empty() {
        return Err(ErrorCode::CorruptionDetected);
    }
    let algo_nb = huf_select_decoder(dst.len(), c_src.len());
    let h_size = if algo_nb != 0 {
        huf_read_dtable_x2(dtable, c_src, wksp_size)?
    } else {
        huf_read_dtable_x1(dtable, c_src, wksp_size)?
    };
    if h_size >= c_src.len() {
        return Err(ErrorCode::SrcSizeWrong);
    }
    let ip = &c_src[h_size..];
    if algo_nb != 0 {
        decompress_4x2_using_dtable(dst, ip, dtable)
    } else {
        decompress_4x1_using_dtable(dst, ip, dtable)
    }
}

/// Whether upstream's build tries the fast four-stream loops first, as
/// `HUF_decompress4X1_usingDTable_internal` and `HUF_decompress4X2_usingDTable_internal`
/// choose (external/zstd/lib/decompress/huf_decompress.c:897-928, 1726-1757 @ zstd v1.5.7).
/// A build with `DYNAMIC_BMI2` (GCC or clang for x86-64 without `-mbmi2`: the Linux wheel)
/// takes them only on a CPU with BMI1 and BMI2 (`ZSTD_cpuSupportsBmi2`), in assembly
/// (`huf_decompress_amd64.S`, the C loop's translation), and the portable body otherwise;
/// any other build (MSVC, where OpenEXR defines `ZSTD_DISABLE_ASM`: the Windows wheel) always
/// takes the C loop. Once per process.
fn fast_decode() -> bool {
    static FAST: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FAST.get_or_init(|| {
        #[cfg(all(target_arch = "x86_64", not(target_env = "msvc")))]
        {
            std::arch::is_x86_feature_detected!("bmi1")
                && std::arch::is_x86_feature_detected!("bmi2")
        }
        #[cfg(not(all(target_arch = "x86_64", not(target_env = "msvc"))))]
        {
            true
        }
    })
}

/// Port of `HUF_decompress4X1_usingDTable_internal`
/// (external/zstd/lib/decompress/huf_decompress.c:897-928 @ zstd v1.5.7): the fast loop when
/// the build takes it ([`fast_decode`]) and it can run, else the portable body.
fn decompress_4x1_using_dtable(dst: &mut [u8], c_src: &[u8], dtable: &HufDTable) -> ZResult<usize> {
    if fast_decode() {
        let ret = decompress_4x1_fast(dst, c_src, dtable)?;
        if ret != 0 {
            return Ok(ret);
        }
    }
    decompress_4x1_using_dtable_body(dst, c_src, dtable)
}

/// Port of `HUF_decompress4X2_usingDTable_internal`
/// (external/zstd/lib/decompress/huf_decompress.c:1726-1757 @ zstd v1.5.7), as the 4X1 one.
fn decompress_4x2_using_dtable(dst: &mut [u8], c_src: &[u8], dtable: &HufDTable) -> ZResult<usize> {
    if fast_decode() {
        let ret = decompress_4x2_fast(dst, c_src, dtable)?;
        if ret != 0 {
            return Ok(ret);
        }
    }
    decompress_4x2_using_dtable_body(dst, c_src, dtable)
}

/// Port of `HUF_DecompressFastArgs` (external/zstd/lib/decompress/huf_decompress.c:160-181
/// @ zstd v1.5.7), its pointers as offsets: `ip` and `iend` into the source (whose start is
/// `ilowest`), `op` and `oend` into the output.
struct FastArgs {
    ip: [usize; 4],
    op: [usize; 4],
    bits: [u64; 4],
    oend: usize,
    iend: [usize; 4],
}

/// Port of `HUF_initFastDStream` (external/zstd/lib/decompress/huf_decompress.c:149-156
/// @ zstd v1.5.7).
fn init_fast_dstream(src: &[u8], ip: usize) -> u64 {
    let last_byte = src[ip + 7];
    let bits_consumed = if last_byte != 0 {
        8 - crate::mem::highbit32(u32::from(last_byte))
    } else {
        0
    };
    let value = crate::mem::read_le64(&src[ip..]) | 1;
    value << bits_consumed
}

/// Port of `HUF_DecompressFastArgs_init` (external/zstd/lib/decompress/huf_decompress.c:183-279
/// @ zstd v1.5.7), on a 64-bit little-endian machine: the arguments, or `None` when the
/// portable body is to be used.
fn fast_args_init(dst_size: usize, src: &[u8], dtable: &HufDTable) -> ZResult<Option<FastArgs>> {
    let src_size = src.len();
    // Avoid nullptr addition
    if dst_size == 0 {
        return Ok(None);
    }
    // strict minimum : jump table + 1 byte per stream
    if src_size < 10 {
        return Err(ErrorCode::CorruptionDetected);
    }
    // Must have at least 8 bytes per stream because we do not handle initializing smaller bit
    // containers. If table log is not correct at this point, fallback to the old decoder.
    if u32::from(dtable.table_log) != HUF_DECODER_FAST_TABLELOG {
        return Ok(None);
    }

    // Read the jump table.
    let length1 = usize::from(read_le16(&src[0..]));
    let length2 = usize::from(read_le16(&src[2..]));
    let length3 = usize::from(read_le16(&src[4..]));
    let length4 = src_size.wrapping_sub(length1 + length2 + length3 + 6);
    let mut iend = [0usize; 4];
    iend[0] = 6; // jumpTable
    iend[1] = iend[0] + length1;
    iend[2] = iend[1] + length2;
    iend[3] = iend[2] + length3;

    // HUF_initFastDStream() requires this, and this small of an input will not benefit from
    // the ASM loop anyways.
    if length1 < 8 || length2 < 8 || length3 < 8 || length4 < 8 {
        return Ok(None);
    }
    if length4 > src_size {
        return Err(ErrorCode::CorruptionDetected); // overflow
    }
    // ip[] contains the position that is currently loaded into bits[].
    let ip = [iend[1] - 8, iend[2] - 8, iend[3] - 8, src_size - 8];

    // op[] contains the output pointers.
    let seg = dst_size.div_ceil(4);
    let op = [0, seg, 2 * seg, 3 * seg];

    // No point to call the ASM loop for tiny outputs.
    if op[3] >= dst_size {
        return Ok(None);
    }

    let bits = [
        init_fast_dstream(src, ip[0]),
        init_fast_dstream(src, ip[1]),
        init_fast_dstream(src, ip[2]),
        init_fast_dstream(src, ip[3]),
    ];
    Ok(Some(FastArgs {
        ip,
        op,
        bits,
        oend: dst_size,
        iend,
    }))
}

/// Port of `HUF_initRemainingDStream` (external/zstd/lib/decompress/huf_decompress.c:281-304
/// @ zstd v1.5.7): stream `stream`'s bit reader after the fast loop.
fn init_remaining_dstream<'a>(
    src: &'a [u8],
    args: &FastArgs,
    stream: usize,
    segment_end: usize,
) -> ZResult<BitDStream<'a>> {
    // Validate that we have not overwritten.
    if args.op[stream] > segment_end {
        return Err(ErrorCode::CorruptionDetected);
    }
    // Validate that we have not read beyond iend[]. Note that ip[] may be < iend[] because
    // the MSB is the next bit to read, and we may have consumed 100% of the stream, so down
    // to iend[i] - 8 is valid.
    if (args.ip[stream] as isize) < args.iend[stream] as isize - 8 {
        return Err(ErrorCode::CorruptionDetected);
    }
    Ok(BitDStream::from_parts(
        src,
        args.ip[stream],
        crate::mem::read_le64(&src[args.ip[stream]..]),
        args.bits[stream].trailing_zeros(),
    ))
}

/// Port of `HUF_decompress4X1_usingDTable_internal_fast_c_loop`
/// (external/zstd/lib/decompress/huf_decompress.c:720-831 @ zstd v1.5.7): five symbols per
/// stream per iteration, while no bound can be reached.
fn fast_c_loop_4x1(dst: &mut [u8], src: &[u8], dt: &[DEltX1], args: &mut FastArgs) {
    let (mut bits, mut ip, mut op) = (args.bits, args.ip, args.op);
    let oend = args.oend;
    loop {
        // Each iteration produces 5 output symbols per stream
        let oiters = (oend - op[3]) / 5;
        // Each iteration consumes up to 11 bits * 5 = 55 bits < 7 bytes per stream.
        let iiters = ip[0] / 7;
        // We can safely run iters iterations before running bounds checks
        let iters = oiters.min(iiters);
        let olimit = op[3] + iters * 5;

        // Exit fast decoding loop once we reach the end.
        if op[3] == olimit {
            break;
        }
        // Exit the decoding loop if any input pointer has crossed the previous one.
        if (1..4).any(|s| ip[s] < ip[s - 1]) {
            break;
        }

        loop {
            // Decode 5 symbols in each of the 4 streams
            for symbol in 0..5 {
                for s in 0..4 {
                    let e = dt[(bits[s] >> 53) as usize];
                    bits[s] <<= u32::from(e.nb_bits) & 0x3F;
                    dst[op[s] + symbol] = e.byte;
                }
            }
            // Reload each of the 4 the bitstreams
            for s in 0..4 {
                let ctz = bits[s].trailing_zeros();
                let nb_bits = ctz & 7;
                let nb_bytes = (ctz >> 3) as usize;
                op[s] += 5;
                ip[s] -= nb_bytes;
                bits[s] = crate::mem::read_le64(&src[ip[s]..]) | 1;
                bits[s] <<= nb_bits;
            }
            if op[3] >= olimit {
                break;
            }
        }
    }
    args.bits = bits;
    args.ip = ip;
    args.op = op;
}

/// The end of each output segment, as the fast decoders' finishing loop steps them.
fn segment_ends(dst_size: usize) -> [usize; 4] {
    let segment_size = dst_size.div_ceil(4);
    let mut segment_end = 0usize;
    let mut ends = [0usize; 4];
    for e in &mut ends {
        if segment_size <= dst_size - segment_end {
            segment_end += segment_size;
        } else {
            segment_end = dst_size;
        }
        *e = segment_end;
    }
    ends
}

/// Port of `HUF_decompress4X1_usingDTable_internal_fast`
/// (external/zstd/lib/decompress/huf_decompress.c:833-893 @ zstd v1.5.7): the fast loop, then
/// each stream finished by `HUF_decodeStreamX1`; 0 when the portable body is to be used.
fn decompress_4x1_fast(dst: &mut [u8], src: &[u8], dtable: &HufDTable) -> ZResult<usize> {
    let dst_size = dst.len();
    let Some(mut args) = fast_args_init(dst_size, src, dtable)? else {
        return Ok(0);
    };
    fast_c_loop_4x1(dst, src, &dtable.x1, &mut args);

    // finish bit streams one by one.
    for (i, segment_end) in segment_ends(dst_size).into_iter().enumerate() {
        let mut bit = init_remaining_dstream(src, &args, i, segment_end)?;
        // Decompress and validate that we have produced exactly the expected length.
        decode_stream_x1(
            dst,
            args.op[i],
            &mut bit,
            segment_end,
            &dtable.x1,
            HUF_DECODER_FAST_TABLELOG,
        )?;
        args.op[i] = segment_end;
    }
    // decoded size
    Ok(dst_size)
}

/// Port of `HUF_decompress4X2_usingDTable_internal_fast_c_loop`
/// (external/zstd/lib/decompress/huf_decompress.c:1523-1664 @ zstd v1.5.7): five lookups per
/// stream per iteration (one or two symbols each), the last stream decoded between the
/// reloads.
fn fast_c_loop_4x2(dst: &mut [u8], src: &[u8], dt: &[DEltX2], args: &mut FastArgs) {
    let (mut bits, mut ip, mut op) = (args.bits, args.ip, args.op);
    let oend = [op[1], op[2], op[3], args.oend];

    // HUF_4X2_DECODE_SYMBOL: MEM_write16 of the cell's sequence, which may write one byte
    // more than the cell's length
    let decode = |s: usize, bits: &mut [u64; 4], op: &mut [usize; 4], dst: &mut [u8]| {
        let e = dt[(bits[s] >> 53) as usize];
        dst[op[s]..op[s] + 2].copy_from_slice(&e.sequence.to_le_bytes());
        bits[s] <<= u32::from(e.nb_bits) & 0x3F;
        op[s] += usize::from(e.length);
    };

    loop {
        // We can consume up to 7 bytes of input per iteration per stream.
        let mut iters = ip[0] / 7;
        // Each iteration can produce up to 10 bytes of output per stream.
        for s in 0..4 {
            let oiters = (oend[s] - op[s]) / 10;
            iters = iters.min(oiters);
        }
        let olimit = op[3] + iters * 5;

        // Exit the fast decoding loop once we reach the end.
        if op[3] == olimit {
            break;
        }
        // Exit the decoding loop if any input pointer has crossed the previous one.
        if (1..4).any(|s| ip[s] < ip[s - 1]) {
            break;
        }

        loop {
            // Decode 5 symbols from each of the first 3 streams.
            for _ in 0..5 {
                for s in 0..3 {
                    decode(s, &mut bits, &mut op, dst);
                }
            }
            // Decode one symbol from the final stream
            decode(3, &mut bits, &mut op, dst);
            // Decode 4 symbols from the final stream & reload bitstreams.
            for s in 0..4 {
                decode(3, &mut bits, &mut op, dst);
                let ctz = bits[s].trailing_zeros();
                let nb_bits = ctz & 7;
                let nb_bytes = (ctz >> 3) as usize;
                ip[s] -= nb_bytes;
                bits[s] = crate::mem::read_le64(&src[ip[s]..]) | 1;
                bits[s] <<= nb_bits;
            }
            if op[3] >= olimit {
                break;
            }
        }
    }
    args.bits = bits;
    args.ip = ip;
    args.op = op;
}

/// Port of `HUF_decompress4X2_usingDTable_internal_fast`
/// (external/zstd/lib/decompress/huf_decompress.c:1666-1724 @ zstd v1.5.7), as the 4X1 one
/// with `HUF_decodeStreamX2`.
fn decompress_4x2_fast(dst: &mut [u8], src: &[u8], dtable: &HufDTable) -> ZResult<usize> {
    let dst_size = dst.len();
    let Some(mut args) = fast_args_init(dst_size, src, dtable)? else {
        return Ok(0);
    };
    fast_c_loop_4x2(dst, src, &dtable.x2, &mut args);

    // finish bitStreams one by one
    for (i, segment_end) in segment_ends(dst_size).into_iter().enumerate() {
        let mut bit = init_remaining_dstream(src, &args, i, segment_end)?;
        decode_stream_x2(
            dst,
            args.op[i],
            &mut bit,
            segment_end,
            &dtable.x2,
            HUF_DECODER_FAST_TABLELOG,
        )?;
        args.op[i] = segment_end;
    }
    // decoded size
    Ok(dst_size)
}
