// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_compress_sequences.c`
//! (external/zstd/lib/compress/zstd_compress_sequences.c:1-442 @ zstd v1.5.7): the choice of
//! each sequence code's table, its description, and the sequences' bit stream. The BMI2
//! build runs the same code.

use crate::bitstream::BitCStream;
use crate::error_private::{ErrorCode, ZResult};
use crate::fse_compress::{
    FSE_NCOUNTBOUND, FseCState, FseCTable, fse_build_ctable, fse_build_ctable_rle,
    fse_build_ctable_workspace_size, fse_normalize_count, fse_optimal_table_log, fse_write_ncount,
};
use crate::zstd_compress_internal::{
    FseRepeat, MAX_FSE_LOG, SET_BASIC, SET_COMPRESSED, SET_REPEAT, SET_RLE, SeqDef, Strategy,
    SymbolEncodingType, ZSTD_LAZY,
};
use crate::zstd_decompress_internal::{
    LL_BITS, LL_FSE_LOG, MAX_SEQ, ML_BITS, ML_FSE_LOG, OFF_FSE_LOG,
};

/// `kInverseProbabilityLog256` (external/zstd/lib/compress/zstd_compress_sequences.c:21-44
/// @ zstd v1.5.7): `floor (-log2 (x / 256) * 256)`, 0 for `x == 0`.
#[rustfmt::skip]
const K_INVERSE_PROBABILITY_LOG256: [u32; 256] = [
    0,    2048, 1792, 1642, 1536, 1453, 1386, 1329, 1280, 1236, 1197, 1162,
    1130, 1100, 1073, 1047, 1024, 1001, 980,  960,  941,  923,  906,  889,
    874,  859,  844,  830,  817,  804,  791,  779,  768,  756,  745,  734,
    724,  714,  704,  694,  685,  676,  667,  658,  650,  642,  633,  626,
    618,  610,  603,  595,  588,  581,  574,  567,  561,  554,  548,  542,
    535,  529,  523,  517,  512,  506,  500,  495,  489,  484,  478,  473,
    468,  463,  458,  453,  448,  443,  438,  434,  429,  424,  420,  415,
    411,  407,  402,  398,  394,  390,  386,  382,  377,  373,  370,  366,
    362,  358,  354,  350,  347,  343,  339,  336,  332,  329,  325,  322,
    318,  315,  311,  308,  305,  302,  298,  295,  292,  289,  286,  282,
    279,  276,  273,  270,  267,  264,  261,  258,  256,  253,  250,  247,
    244,  241,  239,  236,  233,  230,  228,  225,  222,  220,  217,  215,
    212,  209,  207,  204,  202,  199,  197,  194,  192,  190,  187,  185,
    182,  180,  178,  175,  173,  171,  168,  166,  164,  162,  159,  157,
    155,  153,  151,  149,  146,  144,  142,  140,  138,  136,  134,  132,
    130,  128,  126,  123,  121,  119,  117,  115,  114,  112,  110,  108,
    106,  104,  102,  100,  98,   96,   94,   93,   91,   89,   87,   85,
    83,   82,   80,   78,   76,   74,   73,   71,   69,   67,   66,   64,
    62,   61,   59,   57,   55,   54,   52,   50,   49,   47,   46,   44,
    42,   41,   39,   37,   36,   34,   33,   31,   30,   28,   26,   25,
    23,   22,   20,   19,   17,   16,   14,   13,   11,   10,   8,    7,
    5,    4,    2,    1,
];

/// `STREAM_ACCUMULATOR_MIN` on 64-bit (external/zstd/lib/common/bitstream.h:45 @ zstd v1.5.7).
const STREAM_ACCUMULATOR_MIN: u32 = 57;

/// `ERROR (code)` as the `size_t` upstream compares costs with.
#[inline]
fn error_size(e: ErrorCode) -> usize {
    (e as usize).wrapping_neg()
}

/// Port of `ZSTD_useLowProbCount` (external/zstd/lib/compress/zstd_compress_sequences.c:57-64
/// @ zstd v1.5.7).
#[inline]
fn zstd_use_low_prob_count(nb_seq: usize) -> bool {
    // Heuristic: This should cover most blocks <= 16K and start to fade out after 16K to
    // about 32K depending on compressibility.
    nb_seq >= 2048
}

/// Port of `ZSTD_NCountCost` (external/zstd/lib/compress/zstd_compress_sequences.c:70-78
/// @ zstd v1.5.7): the bytes of the normalized count header.
fn zstd_ncount_cost(count: &[u32], max: u32, nb_seq: usize, fse_log: u32) -> ZResult<usize> {
    let mut wksp = [0u8; FSE_NCOUNTBOUND];
    let mut norm = [0i16; MAX_SEQ + 1];
    let table_log = fse_optimal_table_log(fse_log, nb_seq, max);
    fse_normalize_count(
        &mut norm,
        table_log,
        count,
        nb_seq,
        max,
        zstd_use_low_prob_count(nb_seq),
    )?;
    fse_write_ncount(&mut wksp, &norm, max, table_log)
}

/// Port of `ZSTD_entropyCost` (external/zstd/lib/compress/zstd_compress_sequences.c:84-98
/// @ zstd v1.5.7): the bits of `count` at the entropy bound.
fn zstd_entropy_cost(count: &[u32], max: u32, total: usize) -> usize {
    let mut cost: u32 = 0;
    for &c in &count[..=max as usize] {
        let mut norm = (256u32.wrapping_mul(c) as usize / total) as u32;
        if c != 0 && norm == 0 {
            norm = 1;
        }
        cost = cost.wrapping_add(c.wrapping_mul(K_INVERSE_PROBABILITY_LOG256[norm as usize]));
    }
    (cost >> 8) as usize
}

/// Port of `FSE_bitCost` (external/zstd/lib/common/fse.h:485-500 @ zstd v1.5.7): a symbol's
/// approximate cost, with `accuracy_log` fractional bits.
#[inline]
fn fse_bit_cost(ct: &FseCTable, table_log: u32, symbol_value: u32, accuracy_log: u32) -> u32 {
    let (_, delta_nb_bits) = ct.symbol_tt(symbol_value as usize);
    let min_nb_bits = delta_nb_bits >> 16;
    let threshold = (min_nb_bits + 1) << 16;
    let table_size = 1u32 << table_log;
    let delta_from_threshold = threshold.wrapping_sub(delta_nb_bits.wrapping_add(table_size));
    // linear interpolation (very approximate)
    let normalized_delta_from_threshold = (delta_from_threshold << accuracy_log) >> table_log;
    let bit_multiplier = 1u32 << accuracy_log;
    ((min_nb_bits + 1) * bit_multiplier).wrapping_sub(normalized_delta_from_threshold)
}

/// Port of `ZSTD_fseBitCost` (external/zstd/lib/compress/zstd_compress_sequences.c:104-132
/// @ zstd v1.5.7): the bits of `count` with the table, or an error when it lacks a symbol.
pub(crate) fn zstd_fse_bit_cost(ctable: &FseCTable, count: &[u32], max: u32) -> ZResult<usize> {
    const K_ACCURACY_LOG: u32 = 8;
    let mut cost: usize = 0;
    let cstate = FseCState::init(ctable);
    if ctable.max_symbol_value() < max {
        return Err(ErrorCode::Generic);
    }
    for s in 0..=max {
        let table_log = cstate.state_log;
        let bad_cost = (table_log + 1) << K_ACCURACY_LOG;
        let bit_cost = fse_bit_cost(ctable, table_log, s, K_ACCURACY_LOG);
        if count[s as usize] == 0 {
            continue;
        }
        if bit_cost >= bad_cost {
            return Err(ErrorCode::Generic);
        }
        cost = cost.wrapping_add((count[s as usize] as usize).wrapping_mul(bit_cost as usize));
    }
    Ok(cost >> K_ACCURACY_LOG)
}

/// Port of `ZSTD_crossEntropyCost` (external/zstd/lib/compress/zstd_compress_sequences.c:139-154
/// @ zstd v1.5.7): the bits of `count` with the table of `norm`.
pub(crate) fn zstd_cross_entropy_cost(
    norm: &[i16],
    accuracy_log: u32,
    count: &[u32],
    max: u32,
) -> usize {
    let shift = 8 - accuracy_log;
    let mut cost: usize = 0;
    for s in 0..=max as usize {
        let norm_acc = if norm[s] != -1 { norm[s] as u32 } else { 1 };
        let norm256 = norm_acc << shift;
        cost = cost.wrapping_add(
            count[s].wrapping_mul(K_INVERSE_PROBABILITY_LOG256[norm256 as usize]) as usize,
        );
    }
    cost >> 8
}

/// Port of `ZSTD_DefaultPolicy_e` (external/zstd/lib/compress/zstd_compress_sequences.h:18-21
/// @ zstd v1.5.7): whether the default table may be chosen.
pub(crate) type DefaultPolicy = bool;

/// Port of `ZSTD_selectEncodingType` (external/zstd/lib/compress/zstd_compress_sequences.c:156-235
/// @ zstd v1.5.7): RLE, the default table, the previous table or a new one.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zstd_select_encoding_type(
    repeat_mode: &mut FseRepeat,
    count: &[u32],
    max: u32,
    most_frequent: usize,
    nb_seq: usize,
    fse_log: u32,
    prev_ctable: &FseCTable,
    default_norm: &[i16],
    default_norm_log: u32,
    is_default_allowed: DefaultPolicy,
    strategy: Strategy,
) -> SymbolEncodingType {
    if most_frequent == nb_seq {
        *repeat_mode = FseRepeat::None;
        if is_default_allowed && nb_seq <= 2 {
            // Prefer set_basic over set_rle when there are 2 or fewer symbols, since RLE uses
            // 1 byte, but set_basic uses 5-6 bits per symbol. If basic encoding isn't
            // possible, always choose RLE.
            return SET_BASIC;
        }
        return SET_RLE;
    }
    if strategy < ZSTD_LAZY {
        if is_default_allowed {
            let static_fse_nb_seq_max: usize = 1000;
            let mult = 10 - strategy as usize;
            let base_log: usize = 3;
            // 28-36 for offset, 56-72 for lengths
            let dynamic_fse_nb_seq_min = ((1usize << default_norm_log) * mult) >> base_log;
            if (*repeat_mode == FseRepeat::Valid) && (nb_seq < static_fse_nb_seq_max) {
                return SET_REPEAT;
            }
            if (nb_seq < dynamic_fse_nb_seq_min)
                || (most_frequent < (nb_seq >> (default_norm_log - 1)))
            {
                // The format allows default tables to be repeated, but it isn't useful.
                *repeat_mode = FseRepeat::None;
                return SET_BASIC;
            }
        }
    } else {
        let basic_cost = if is_default_allowed {
            zstd_cross_entropy_cost(default_norm, default_norm_log, count, max)
        } else {
            error_size(ErrorCode::Generic)
        };
        let repeat_cost = if *repeat_mode != FseRepeat::None {
            zstd_fse_bit_cost(prev_ctable, count, max).unwrap_or_else(error_size)
        } else {
            error_size(ErrorCode::Generic)
        };
        let ncount_cost = zstd_ncount_cost(count, max, nb_seq, fse_log).unwrap_or_else(error_size);
        let compressed_cost =
            (ncount_cost << 3).wrapping_add(zstd_entropy_cost(count, max, nb_seq));
        if basic_cost <= repeat_cost && basic_cost <= compressed_cost {
            *repeat_mode = FseRepeat::None;
            return SET_BASIC;
        }
        if repeat_cost <= compressed_cost {
            return SET_REPEAT;
        }
    }
    *repeat_mode = FseRepeat::Check;
    SET_COMPRESSED
}

/// Port of `ZSTD_buildCTable` (external/zstd/lib/compress/zstd_compress_sequences.c:242-288
/// @ zstd v1.5.7): `next_ctable` for the chosen type, its description into `dst`; returns
/// the description's size. `count` loses the last symbol's count when the table is new.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zstd_build_ctable(
    dst: &mut [u8],
    next_ctable: &mut FseCTable,
    fse_log: u32,
    type_: SymbolEncodingType,
    count: &mut [u32],
    max: u32,
    code_table: &[u8],
    default_norm: &[i16],
    default_norm_log: u32,
    default_max: u32,
    prev_ctable: &FseCTable,
    entropy_workspace_size: usize,
) -> ZResult<usize> {
    let nb_seq = code_table.len();
    match type_ {
        SET_RLE => {
            fse_build_ctable_rle(next_ctable, max as u8);
            if dst.is_empty() {
                return Err(ErrorCode::DstSizeTooSmall); // not enough space
            }
            dst[0] = code_table[0];
            Ok(1)
        }
        SET_REPEAT => {
            next_ctable.0.copy_from_slice(&prev_ctable.0);
            Ok(0)
        }
        SET_BASIC => {
            // note : could be pre-calculated
            fse_build_ctable(
                next_ctable,
                default_norm,
                default_max,
                default_norm_log,
                entropy_workspace_size,
            )?;
            Ok(0)
        }
        _ => {
            // set_compressed
            let mut norm = [0i16; MAX_SEQ + 1];
            let mut nb_seq_1 = nb_seq;
            let table_log = fse_optimal_table_log(fse_log, nb_seq, max);
            let last = usize::from(code_table[nb_seq - 1]);
            if count[last] > 1 {
                count[last] -= 1;
                nb_seq_1 -= 1;
            }
            fse_normalize_count(
                &mut norm,
                table_log,
                count,
                nb_seq_1,
                max,
                zstd_use_low_prob_count(nb_seq_1),
            )?;
            // overflow protected
            let ncount_size = fse_write_ncount(dst, &norm, max, table_log)?;
            fse_build_ctable(
                next_ctable,
                &norm,
                max,
                table_log,
                fse_build_ctable_workspace_size(MAX_SEQ as u32, MAX_FSE_LOG),
            )?;
            Ok(ncount_size)
        }
    }
}

/// Port of `ZSTD_encodeSequences_body`
/// (external/zstd/lib/compress/zstd_compress_sequences.c:290-382 @ zstd v1.5.7), on 64-bit,
/// with `ZSTD_encodeSequences` (lines 419-442): the sequences' bit stream, from the last.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zstd_encode_sequences(
    dst: &mut [u8],
    ctable_match_length: &FseCTable,
    ml_code_table: &[u8],
    ctable_offset_bits: &FseCTable,
    of_code_table: &[u8],
    ctable_lit_length: &FseCTable,
    ll_code_table: &[u8],
    sequences: &[SeqDef],
    long_offsets: bool,
) -> ZResult<usize> {
    let nb_seq = sequences.len();
    let Ok(mut block_stream) = BitCStream::init(dst) else {
        return Err(ErrorCode::DstSizeTooSmall); // not enough space remaining
    };

    // first symbols
    let last = nb_seq - 1;
    let mut state_match_length =
        FseCState::init2(ctable_match_length, u32::from(ml_code_table[last]));
    let mut state_offset_bits =
        FseCState::init2(ctable_offset_bits, u32::from(of_code_table[last]));
    let mut state_lit_length = FseCState::init2(ctable_lit_length, u32::from(ll_code_table[last]));
    block_stream.add_bits(
        u64::from(sequences[last].lit_length),
        u32::from(LL_BITS[usize::from(ll_code_table[last])]),
    );
    block_stream.add_bits(
        u64::from(sequences[last].ml_base),
        u32::from(ML_BITS[usize::from(ml_code_table[last])]),
    );
    if long_offsets {
        let of_bits = u32::from(of_code_table[last]);
        let m = STREAM_ACCUMULATOR_MIN - 1;
        let extra_bits = of_bits - if of_bits < m { of_bits } else { m };
        if extra_bits != 0 {
            block_stream.add_bits(u64::from(sequences[last].off_base), extra_bits);
            block_stream.flush_bits();
        }
        block_stream.add_bits(
            u64::from(sequences[last].off_base >> extra_bits),
            of_bits - extra_bits,
        );
    } else {
        block_stream.add_bits(
            u64::from(sequences[last].off_base),
            u32::from(of_code_table[last]),
        );
    }
    block_stream.flush_bits();

    for n in (0..nb_seq - 1).rev() {
        let ll_code = ll_code_table[n];
        let of_code = of_code_table[n];
        let ml_code = ml_code_table[n];
        let ll_bits = u32::from(LL_BITS[usize::from(ll_code)]);
        let of_bits = u32::from(of_code);
        let ml_bits = u32::from(ML_BITS[usize::from(ml_code)]);
        state_offset_bits.encode_symbol(&mut block_stream, ctable_offset_bits, u32::from(of_code)); // 15
        state_match_length.encode_symbol(
            &mut block_stream,
            ctable_match_length,
            u32::from(ml_code),
        ); // 24
        state_lit_length.encode_symbol(&mut block_stream, ctable_lit_length, u32::from(ll_code)); // 33
        if of_bits + ml_bits + ll_bits >= 64 - 7 - (LL_FSE_LOG + ML_FSE_LOG + OFF_FSE_LOG) {
            block_stream.flush_bits(); // (7)
        }
        block_stream.add_bits(u64::from(sequences[n].lit_length), ll_bits);
        block_stream.add_bits(u64::from(sequences[n].ml_base), ml_bits);
        if of_bits + ml_bits + ll_bits > 56 {
            block_stream.flush_bits();
        }
        if long_offsets {
            let m = STREAM_ACCUMULATOR_MIN - 1;
            let extra_bits = of_bits - if of_bits < m { of_bits } else { m };
            if extra_bits != 0 {
                block_stream.add_bits(u64::from(sequences[n].off_base), extra_bits);
                block_stream.flush_bits(); // (7)
            }
            block_stream.add_bits(
                u64::from(sequences[n].off_base >> extra_bits),
                of_bits - extra_bits,
            ); // 31
        } else {
            block_stream.add_bits(u64::from(sequences[n].off_base), of_bits); // 31
        }
        block_stream.flush_bits(); // (7)
    }

    state_match_length.flush(&mut block_stream);
    state_offset_bits.flush(&mut block_stream);
    state_lit_length.flush(&mut block_stream);

    let stream_size = block_stream.close();
    if stream_size == 0 {
        return Err(ErrorCode::DstSizeTooSmall); // not enough space
    }
    Ok(stream_size)
}
