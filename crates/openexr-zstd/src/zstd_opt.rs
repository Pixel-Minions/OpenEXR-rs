// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_opt.c` (external/zstd/lib/compress/zstd_opt.c:1-1580 @ zstd v1.5.7): the
//! optimal parsers (btopt, btultra, btultra2) over the binary tree, without a dictionary
//! (`ZSTD_compressCCtx` uses no other). Long distance matching, which a window of 2^27 bytes
//! or more turns on and OpenEXR's chunks never reach, and the dictionary-seeded statistics,
//! which only a dictionary makes valid at a frame's first block, are n/a; `ZSTD_C_PREDICT`,
//! not defined in upstream's build, is too.

use crate::hist::hist_count_simple;
use crate::mem::highbit32;
use crate::zstd_compress_internal::{
    MatchState, OptMatch, OptPrice, OptState, Optimal, SeqStore, ZSTD_OPT_NUM,
    get_lowest_match_index, hash_ptr, ll_code, ml_code, offset_to_offbase, read32, update_rep,
    zstd_count,
};
use crate::zstd_decompress_internal::{LL_BITS, MAX_LL, MAX_ML, MAX_OFF, ML_BITS};

/// `ZSTD_LITFREQ_ADD` (external/zstd/lib/compress/zstd_opt.c:19 @ zstd v1.5.7): scaling
/// factor for litFreq, so that frequencies adapt faster to new stats.
const ZSTD_LITFREQ_ADD: u32 = 2;
/// `ZSTD_MAX_PRICE` (external/zstd/lib/compress/zstd_opt.c:20 @ zstd v1.5.7).
const ZSTD_MAX_PRICE: i32 = 1 << 30;
/// `ZSTD_PREDEF_THRESHOLD` (external/zstd/lib/compress/zstd_opt.c:22 @ zstd v1.5.7): below
/// it, symbols' costs come from pre-defined distributions.
const ZSTD_PREDEF_THRESHOLD: usize = 8;
/// `BITCOST_ACCURACY` and `BITCOST_MULTIPLIER` (external/zstd/lib/compress/zstd_opt.c:37-38
/// @ zstd v1.5.7).
const BITCOST_ACCURACY: u32 = 8;
const BITCOST_MULTIPLIER: u32 = 1 << BITCOST_ACCURACY;
/// `MaxLit` (external/zstd/lib/common/zstd_internal.h:90 @ zstd v1.5.7).
const MAX_LIT: u32 = 255;
/// `ZSTD_BLOCKSIZE_MAX` (external/zstd/lib/zstd.h:148 @ zstd v1.5.7).
const ZSTD_BLOCKSIZE_MAX: u32 = 1 << 17;
/// `prime3bytes` (external/zstd/lib/compress/zstd_compress_internal.h:898 @ zstd v1.5.7).
const PRIME3BYTES: u32 = 506832829;

/// Port of `ZSTD_hash3Ptr` (external/zstd/lib/compress/zstd_compress_internal.h:899-900
/// @ zstd v1.5.7).
#[inline]
fn hash3_ptr(src: &[u8], p: usize, h: u32) -> usize {
    ((read32(src, p) << (32 - 24)).wrapping_mul(PRIME3BYTES) >> (32 - h)) as usize
}

/// Port of `ZSTD_bitWeight` (external/zstd/lib/compress/zstd_opt.c:45-48 @ zstd v1.5.7): the
/// cost of a stat in full bits.
#[inline]
fn bit_weight(stat: u32) -> u32 {
    highbit32(stat.wrapping_add(1)) * BITCOST_MULTIPLIER
}

/// Port of `ZSTD_fracWeight` (external/zstd/lib/compress/zstd_opt.c:53-65 @ zstd v1.5.7): the
/// fractional-bit cost of a stat, by linear interpolation.
#[inline]
fn frac_weight(raw_stat: u32) -> u32 {
    let stat = raw_stat.wrapping_add(1);
    let hb = highbit32(stat);
    let b_weight = hb * BITCOST_MULTIPLIER;
    // a value between 1 and 2 in fixed point
    let f_weight = (stat << BITCOST_ACCURACY) >> hb;
    b_weight + f_weight
}

/// `WEIGHT(stat, opt)` (external/zstd/lib/compress/zstd_opt.c:39 @ zstd v1.5.7): the
/// fractional weight for the higher levels, the bit weight for level 0.
#[inline]
fn weight(stat: u32, opt_level: i32) -> u32 {
    if opt_level != 0 {
        frac_weight(stat)
    } else {
        bit_weight(stat)
    }
}

/// Port of `ZSTD_setBasePrices` (external/zstd/lib/compress/zstd_opt.c:82-89 @ zstd v1.5.7),
/// literals compressed (`ZSTD_compressedLiterals`: the mode is never "disable" here).
fn set_base_prices(opt: &mut OptState, opt_level: i32) {
    opt.lit_sum_base_price = weight(opt.lit_sum, opt_level);
    opt.lit_length_sum_base_price = weight(opt.lit_length_sum, opt_level);
    opt.match_length_sum_base_price = weight(opt.match_length_sum, opt_level);
    opt.off_code_sum_base_price = weight(opt.off_code_sum, opt_level);
}

/// Port of `sum_u32` (external/zstd/lib/compress/zstd_opt.c:92-100 @ zstd v1.5.7).
fn sum_u32(table: &[u32]) -> u32 {
    table.iter().fold(0u32, |t, &v| t.wrapping_add(v))
}

/// Port of `ZSTD_downscaleStats` (external/zstd/lib/compress/zstd_opt.c:104-121
/// @ zstd v1.5.7): each stat shifted down, kept at 1 at least (`base1`) or when present.
fn downscale_stats(table: &mut [u32], last_elt_index: u32, shift: u32, base1: bool) -> u32 {
    let mut sum = 0u32;
    for s in &mut table[..=last_elt_index as usize] {
        let base = if base1 { 1 } else { u32::from(*s > 0) };
        let new_stat = base + (*s >> shift);
        sum = sum.wrapping_add(new_stat);
        *s = new_stat;
    }
    sum
}

/// Port of `ZSTD_scaleStats` (external/zstd/lib/compress/zstd_opt.c:126-138 @ zstd v1.5.7):
/// the stats reduced when their sum is too large; the resulting sum.
fn scale_stats(table: &mut [u32], last_elt_index: u32, log_target: u32) -> u32 {
    let prevsum = sum_u32(&table[..=last_elt_index as usize]);
    let factor = prevsum >> log_target;
    if factor <= 1 {
        return prevsum;
    }
    downscale_stats(table, last_elt_index, highbit32(factor), true)
}

/// Port of `ZSTD_rescaleFreqs` (external/zstd/lib/compress/zstd_opt.c:140-264 @ zstd v1.5.7),
/// without a dictionary: at a frame's first block (no literal length stats yet) the stats
/// initialized, the literals' from the block itself; at the next ones, the stats downscaled.
fn rescale_freqs(opt: &mut OptState, src: &[u8], opt_level: i32) {
    opt.price_type = OptPrice::Dynamic;

    if opt.lit_length_sum == 0 {
        // no literals stats collected -> first block assumed -> init

        // heuristic: use pre-defined stats for too small inputs
        if src.len() <= ZSTD_PREDEF_THRESHOLD {
            opt.price_type = OptPrice::Predef;
        }

        // (Huffman stats from a dictionary: the previous block's table is never valid at a
        // frame's first block without one.)

        // first block, no dictionary: base initial cost of literals on direct frequency within src
        let mut lit = MAX_LIT;
        hist_count_simple(&mut opt.lit_freq, &mut lit, src); // use raw first block to init statistics
        opt.lit_sum = downscale_stats(&mut opt.lit_freq, MAX_LIT, 8, false);

        const BASE_LL_FREQS: [u32; MAX_LL as usize + 1] = [
            4, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1, 1, 1, 1, 1,
        ];
        opt.lit_length_freq.copy_from_slice(&BASE_LL_FREQS);
        opt.lit_length_sum = sum_u32(&BASE_LL_FREQS);

        for f in opt.match_length_freq.iter_mut() {
            *f = 1;
        }
        opt.match_length_sum = MAX_ML + 1;

        const BASE_OFC_FREQS: [u32; MAX_OFF as usize + 1] = [
            6, 2, 1, 1, 2, 3, 4, 4, 4, 3, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
            1, 1, 1,
        ];
        opt.off_code_freq.copy_from_slice(&BASE_OFC_FREQS);
        opt.off_code_sum = sum_u32(&BASE_OFC_FREQS);
    } else {
        // new block : scale down accumulated statistics
        opt.lit_sum = scale_stats(&mut opt.lit_freq, MAX_LIT, 12);
        opt.lit_length_sum = scale_stats(&mut opt.lit_length_freq, MAX_LL, 11);
        opt.match_length_sum = scale_stats(&mut opt.match_length_freq, MAX_ML, 11);
        opt.off_code_sum = scale_stats(&mut opt.off_code_freq, MAX_OFF, 11);
    }

    set_base_prices(opt, opt_level);
}

/// Port of `ZSTD_rawLiteralsCost` (external/zstd/lib/compress/zstd_opt.c:266-292
/// @ zstd v1.5.7): the price of `literals` (only), in the `U32` arithmetic of upstream.
fn raw_literals_cost(literals: &[u8], opt: &OptState, opt_level: i32) -> u32 {
    let lit_length = literals.len() as u32;
    if lit_length == 0 {
        return 0;
    }
    if opt.price_type == OptPrice::Predef {
        return (lit_length * 6) * BITCOST_MULTIPLIER; // 6 bit per literal - no statistic used
    }

    // dynamic statistics
    let mut price = opt.lit_sum_base_price.wrapping_mul(lit_length);
    let lit_price_max = opt.lit_sum_base_price.wrapping_sub(BITCOST_MULTIPLIER);
    for &l in literals {
        let mut lit_price = weight(opt.lit_freq[l as usize], opt_level);
        if lit_price > lit_price_max {
            lit_price = lit_price_max;
        }
        price = price.wrapping_sub(lit_price);
    }
    price
}

/// Port of `ZSTD_litLengthPrice` (external/zstd/lib/compress/zstd_opt.c:295-317 @ zstd v1.5.7):
/// the cost of the literal length symbol.
fn lit_length_price(lit_length: u32, opt: &OptState, opt_level: i32) -> u32 {
    if opt.price_type == OptPrice::Predef {
        return weight(lit_length, opt_level);
    }

    // ZSTD_LLcode() can't compute litLength price for sizes >= ZSTD_BLOCKSIZE_MAX: pretend it
    // would cost 1 bit more than ZSTD_BLOCKSIZE_MAX - 1.
    if lit_length == ZSTD_BLOCKSIZE_MAX {
        return BITCOST_MULTIPLIER.wrapping_add(lit_length_price(
            ZSTD_BLOCKSIZE_MAX - 1,
            opt,
            opt_level,
        ));
    }

    // dynamic statistics
    let ll_c = ll_code(lit_length) as usize;
    (u32::from(LL_BITS[ll_c]) * BITCOST_MULTIPLIER)
        .wrapping_add(opt.lit_length_sum_base_price)
        .wrapping_sub(weight(opt.lit_length_freq[ll_c], opt_level))
}

/// Port of `ZSTD_getMatchPrice` (external/zstd/lib/compress/zstd_opt.c:323-354 @ zstd v1.5.7):
/// the cost of the match part (offset and match length) of a sequence.
fn get_match_price(off_base: u32, match_length: u32, opt: &OptState, opt_level: i32) -> u32 {
    let off_code = highbit32(off_base);
    let ml_base = match_length - 3;

    if opt.price_type == OptPrice::Predef {
        // fixed scheme, does not use statistics
        return weight(ml_base, opt_level).wrapping_add((16 + off_code) * BITCOST_MULTIPLIER); // emulated offset cost
    }

    // dynamic statistics
    let mut price = (off_code * BITCOST_MULTIPLIER).wrapping_add(
        opt.off_code_sum_base_price
            .wrapping_sub(weight(opt.off_code_freq[off_code as usize], opt_level)),
    );
    if opt_level < 2 && off_code >= 20 {
        price = price.wrapping_add((off_code - 19) * 2 * BITCOST_MULTIPLIER); // handicap for long distance offsets, favor decompression speed
    }

    // match Length
    let ml_c = ml_code(ml_base) as usize;
    price = price.wrapping_add(
        (u32::from(ML_BITS[ml_c]) * BITCOST_MULTIPLIER).wrapping_add(
            opt.match_length_sum_base_price
                .wrapping_sub(weight(opt.match_length_freq[ml_c], opt_level)),
        ),
    );

    price.wrapping_add(BITCOST_MULTIPLIER / 5) // heuristic : make matches a bit more costly to favor less sequences
}

/// Port of `ZSTD_updateStats` (external/zstd/lib/compress/zstd_opt.c:356-391 @ zstd v1.5.7).
fn update_stats(opt: &mut OptState, literals: &[u8], off_base: u32, match_length: u32) {
    let lit_length = literals.len() as u32;
    // literals
    for &l in literals {
        opt.lit_freq[l as usize] = opt.lit_freq[l as usize].wrapping_add(ZSTD_LITFREQ_ADD);
    }
    opt.lit_sum = opt.lit_sum.wrapping_add(lit_length * ZSTD_LITFREQ_ADD);

    // literal Length
    let ll_c = ll_code(lit_length) as usize;
    opt.lit_length_freq[ll_c] = opt.lit_length_freq[ll_c].wrapping_add(1);
    opt.lit_length_sum = opt.lit_length_sum.wrapping_add(1);

    // offset code : follows storeSeq() numeric representation
    let off_code = highbit32(off_base) as usize;
    opt.off_code_freq[off_code] = opt.off_code_freq[off_code].wrapping_add(1);
    opt.off_code_sum = opt.off_code_sum.wrapping_add(1);

    // match Length
    let ml_c = ml_code(match_length - 3) as usize;
    opt.match_length_freq[ml_c] = opt.match_length_freq[ml_c].wrapping_add(1);
    opt.match_length_sum = opt.match_length_sum.wrapping_add(1);
}

/// Port of `ZSTD_readMINMATCH` (external/zstd/lib/compress/zstd_opt.c:393-404 @ zstd v1.5.7),
/// little-endian: for comparisons only.
#[inline]
fn read_minmatch(src: &[u8], p: usize, length: u32) -> u32 {
    if length == 3 {
        read32(src, p) << 8
    } else {
        read32(src, p)
    }
}

/// Port of `ZSTD_insertAndFindFirstIndexHash3` (external/zstd/lib/compress/zstd_opt.c:409-431
/// @ zstd v1.5.7): `hashTable3` updated up to `ip` (excluded); its entry for `ip`.
fn insert_and_find_first_index_hash3(
    ms: &mut MatchState,
    next_to_update3: &mut u32,
    src: &[u8],
    ip: usize,
) -> u32 {
    let hash_log3 = ms.hash_log3;
    let mut idx = *next_to_update3;
    let target = ms.window.idx(ip);
    let hash3 = hash3_ptr(src, ip, hash_log3);

    while idx < target {
        ms.hash_table3[hash3_ptr(src, ms.window.pos(idx), hash_log3)] = idx;
        idx += 1;
    }

    *next_to_update3 = target;
    ms.hash_table3[hash3]
}

/// A tree pointer: a slot of the tree, or upstream's `dummy32`.
#[derive(Clone, Copy)]
enum BtSlot {
    At(usize),
    Dummy,
}

#[inline]
fn bt_set(bt: &mut [u32], slot: BtSlot, v: u32) {
    if let BtSlot::At(i) = slot {
        bt[i] = v;
    }
}

/// Port of `ZSTD_insertBt1` (external/zstd/lib/compress/zstd_opt.c:439-558 @ zstd v1.5.7),
/// without a dictionary: `ip` added to the tree; the number of positions added (more than one
/// after a long repetitive match).
fn insert_bt1(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    iend: usize,
    target: u32,
    mls: u32,
) -> u32 {
    let cparams = ms.cparams;
    let h = hash_ptr(src, ip, cparams.hash_log, mls);
    let bt_log = cparams.chain_log - 1;
    let bt_mask = (1u32 << bt_log) - 1;
    let mut match_index = ms.hash_table[h];
    let mut common_length_smaller = 0usize;
    let mut common_length_larger = 0usize;
    let curr = ms.window.idx(ip);
    let bt_low = curr.saturating_sub(bt_mask); // btMask >= curr ? 0 : curr - btMask
    let mut smaller_ptr = BtSlot::At(2 * (curr & bt_mask) as usize);
    let mut larger_ptr = BtSlot::At(2 * (curr & bt_mask) as usize + 1);
    // windowLow is based on target because we only need positions that will be in the window
    // at the end of the tree update.
    let window_low = get_lowest_match_index(ms, target, cparams.window_log);
    let mut match_end_idx = curr + 8 + 1;
    let mut best_length = 8usize;
    let mut nb_compares = 1u32 << cparams.search_log;

    ms.hash_table[h] = curr; // Update Hash Table

    while nb_compares != 0 && match_index >= window_low {
        let next_ptr = 2 * (match_index & bt_mask) as usize;
        // guaranteed minimum nb of common bytes
        let mut match_length = common_length_smaller.min(common_length_larger);
        let m = ms.window.pos(match_index);
        match_length += zstd_count(src, ip + match_length, m + match_length, iend);

        if match_length > best_length {
            best_length = match_length;
            if match_length > (match_end_idx - match_index) as usize {
                match_end_idx = match_index + match_length as u32;
            }
        }

        if ip + match_length == iend {
            // equal : no way to know if inf or sup; drop, to guarantee consistency
            break;
        }

        if src[m + match_length] < src[ip + match_length] {
            // match is smaller than current
            bt_set(&mut ms.chain_table, smaller_ptr, match_index); // update smaller idx
            common_length_smaller = match_length; // all smaller will now have at least this guaranteed common length
            if match_index <= bt_low {
                // beyond tree size, stop searching
                smaller_ptr = BtSlot::Dummy;
                break;
            }
            smaller_ptr = BtSlot::At(next_ptr + 1); // new "candidate" => larger than match, which was smaller than target
            match_index = ms.chain_table[next_ptr + 1]; // new matchIndex, larger than previous and closer to current
        } else {
            // match is larger than current
            bt_set(&mut ms.chain_table, larger_ptr, match_index);
            common_length_larger = match_length;
            if match_index <= bt_low {
                // beyond tree size, stop searching
                larger_ptr = BtSlot::Dummy;
                break;
            }
            larger_ptr = BtSlot::At(next_ptr);
            match_index = ms.chain_table[next_ptr];
        }
        nb_compares -= 1;
    }

    bt_set(&mut ms.chain_table, smaller_ptr, 0);
    bt_set(&mut ms.chain_table, larger_ptr, 0);
    let positions = if best_length > 384 {
        192u32.min((best_length - 384) as u32) // speed optimization
    } else {
        0
    };
    positions.max(match_end_idx - (curr + 8))
}

/// Port of `ZSTD_updateTree_internal` (external/zstd/lib/compress/zstd_opt.c:560-581
/// @ zstd v1.5.7), without a dictionary: the positions up to `ip` added to the tree.
fn update_tree_internal(ms: &mut MatchState, src: &[u8], ip: usize, iend: usize, mls: u32) {
    let target = ms.window.idx(ip);
    let mut idx = ms.next_to_update;
    while idx < target {
        let forward = insert_bt1(ms, src, ms.window.pos(idx), iend, target, mls);
        idx += forward;
    }
    ms.next_to_update = target;
}

/// Port of `ZSTD_insertBtAndGetAllMatches` (external/zstd/lib/compress/zstd_opt.c:587-818
/// @ zstd v1.5.7), without a dictionary: `ip` inserted in the tree, and every match longer
/// than the one before, from `length_to_beat` on, stored in `matches` (repcodes first);
/// their number.
#[allow(clippy::too_many_arguments)]
fn insert_bt_and_get_all_matches(
    matches: &mut [OptMatch],
    ms: &mut MatchState,
    next_to_update3: &mut u32,
    src: &[u8],
    ip: usize,
    i_limit: usize,
    rep: &[u32; 3],
    ll0: u32,
    length_to_beat: u32,
    mls: u32,
) -> u32 {
    let cparams = ms.cparams;
    let sufficient_len = cparams.target_length.min(ZSTD_OPT_NUM as u32 - 1);
    let curr = ms.window.idx(ip);
    let min_match = if mls == 3 { 3 } else { 4 };
    let h = hash_ptr(src, ip, cparams.hash_log, mls);
    let mut match_index = ms.hash_table[h];
    let bt_log = cparams.chain_log - 1;
    let bt_mask = (1u32 << bt_log) - 1;
    let mut common_length_smaller = 0usize;
    let mut common_length_larger = 0usize;
    let dict_limit = ms.window.dict_limit;
    let bt_low = curr.saturating_sub(bt_mask); // (btMask >= curr) ? 0 : curr - btMask
    let window_low = get_lowest_match_index(ms, curr, cparams.window_log);
    let match_low = if window_low != 0 { window_low } else { 1 };
    let mut smaller_ptr = BtSlot::At(2 * (curr & bt_mask) as usize);
    let mut larger_ptr = BtSlot::At(2 * (curr & bt_mask) as usize + 1);
    let mut match_end_idx = curr + 8 + 1; // farthest referenced position of any match => detects repetitive patterns
    let mut mnum = 0u32;
    let mut nb_compares = 1u32 << cparams.search_log;

    let mut best_length = (length_to_beat - 1) as usize;

    // check repCode
    {
        let last_r = 3 + ll0;
        for rep_code in ll0..last_r {
            let rep_offset = if rep_code == 3 {
                rep[0].wrapping_sub(1)
            } else {
                rep[rep_code as usize]
            };
            let rep_index = curr.wrapping_sub(rep_offset);
            let mut rep_len = 0u32;
            // intentional overflow, discards 0 and -1: equivalent to `curr > repIndex >= dictLimit`
            // (both reads only once the offset is known inside the prefix, as upstream's
            // nested tests)
            if rep_offset.wrapping_sub(1) < curr - dict_limit
                && ((rep_index >= window_low)
                    & (read_minmatch(src, ip, min_match)
                        == read_minmatch(src, ip - rep_offset as usize, min_match)))
            {
                rep_len = zstd_count(
                    src,
                    ip + min_match as usize,
                    ip + min_match as usize - rep_offset as usize,
                    i_limit,
                ) as u32
                    + min_match;
            }
            // (repIndex < dictLimit || repIndex >= curr: a dictionary's, n/a)
            // save longer solution
            if rep_len as usize > best_length {
                best_length = rep_len as usize;
                matches[mnum as usize] = OptMatch {
                    off: rep_code - ll0 + 1, // REPCODE_TO_OFFBASE: expect value between 1 and 3
                    len: rep_len,
                };
                mnum += 1;
                if (rep_len > sufficient_len) | (ip + rep_len as usize == i_limit) {
                    // best possible
                    return mnum;
                }
            }
        }
    }

    // HC3 match finder
    if mls == 3 && best_length < mls as usize {
        let match_index3 = insert_and_find_first_index_hash3(ms, next_to_update3, src, ip);
        if (match_index3 >= match_low) & (curr.wrapping_sub(match_index3) < (1 << 18)) {
            // heuristic : longer distance likely too expensive
            let mlen = zstd_count(src, ip, ms.window.pos(match_index3), i_limit);

            // save best solution
            if mlen >= mls as usize {
                best_length = mlen;
                matches[0] = OptMatch {
                    off: offset_to_offbase(curr - match_index3),
                    len: mlen as u32,
                };
                mnum = 1;
                if (mlen > sufficient_len as usize) | (ip + mlen == i_limit) {
                    // best possible length
                    ms.next_to_update = curr + 1; // skip insertion
                    return 1;
                }
            }
        }
    }

    ms.hash_table[h] = curr; // Update Hash Table

    while nb_compares != 0 && match_index >= match_low {
        let next_ptr = 2 * (match_index & bt_mask) as usize;
        // guaranteed minimum nb of common bytes
        let mut match_length = common_length_smaller.min(common_length_larger);
        let m = ms.window.pos(match_index);
        match_length += zstd_count(src, ip + match_length, m + match_length, i_limit);

        if match_length > best_length {
            if match_length > (match_end_idx - match_index) as usize {
                match_end_idx = match_index + match_length as u32;
            }
            best_length = match_length;
            matches[mnum as usize] = OptMatch {
                off: offset_to_offbase(curr - match_index),
                len: match_length as u32,
            };
            mnum += 1;
            if (match_length > ZSTD_OPT_NUM) | (ip + match_length == i_limit) {
                // equal : no way to know if inf or sup; drop, to preserve bt consistency
                break;
            }
        }

        if src[m + match_length] < src[ip + match_length] {
            // match smaller than current
            bt_set(&mut ms.chain_table, smaller_ptr, match_index); // update smaller idx
            common_length_smaller = match_length; // all smaller will now have at least this guaranteed common length
            if match_index <= bt_low {
                // beyond tree size, stop the search
                smaller_ptr = BtSlot::Dummy;
                break;
            }
            smaller_ptr = BtSlot::At(next_ptr + 1); // new candidate => larger than match, which was smaller than current
            match_index = ms.chain_table[next_ptr + 1]; // new matchIndex, larger than previous, closer to current
        } else {
            bt_set(&mut ms.chain_table, larger_ptr, match_index);
            common_length_larger = match_length;
            if match_index <= bt_low {
                // beyond tree size, stop the search
                larger_ptr = BtSlot::Dummy;
                break;
            }
            larger_ptr = BtSlot::At(next_ptr);
            match_index = ms.chain_table[next_ptr];
        }
        nb_compares -= 1;
    }

    bt_set(&mut ms.chain_table, smaller_ptr, 0);
    bt_set(&mut ms.chain_table, larger_ptr, 0);

    ms.next_to_update = match_end_idx - 8; // skip repetitive patterns
    mnum
}

/// Port of `ZSTD_btGetAllMatches_internal` (external/zstd/lib/compress/zstd_opt.c:830-850
/// @ zstd v1.5.7), with `ZSTD_selectBtGetAllMatches` (lines 888-905): the tree updated to
/// `ip`, then its matches; none in an area skipped after a long match.
#[allow(clippy::too_many_arguments)]
fn bt_get_all_matches(
    matches: &mut [OptMatch],
    ms: &mut MatchState,
    next_to_update3: &mut u32,
    src: &[u8],
    ip: usize,
    i_high_limit: usize,
    rep: &[u32; 3],
    ll0: u32,
    length_to_beat: u32,
) -> u32 {
    let mls = ms.cparams.min_match.clamp(3, 6); // BOUNDED(3, minMatch, 6)
    if ip < ms.window.pos(ms.next_to_update) {
        return 0; // skipped area
    }
    update_tree_internal(ms, src, ip, i_high_limit, mls);
    insert_bt_and_get_all_matches(
        matches,
        ms,
        next_to_update3,
        src,
        ip,
        i_high_limit,
        rep,
        ll0,
        length_to_beat,
        mls,
    )
}

/// Port of `ZSTD_newRep` (external/zstd/lib/compress/zstd_compress_internal.h:837-845
/// @ zstd v1.5.7).
#[inline]
fn new_rep(rep: &[u32; 3], off_base: u32, ll0: bool) -> [u32; 3] {
    let mut r = *rep;
    update_rep(&mut r, off_base, u32::from(ll0));
    r
}

/// Port of `ZSTD_compressBlock_opt_generic` (external/zstd/lib/compress/zstd_opt.c:1074-1441
/// @ zstd v1.5.7), without a dictionary or long distance matches: the block
/// `src[istart..iend]` parsed by pricing every way of reaching each position from the last
/// match; returns the last literals' size. `opt_level` 0 is btopt's, 2 btultra's.
pub(crate) fn compress_block_opt_generic(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
    opt_level: i32,
) -> usize {
    let mut ip = istart;
    let mut anchor = istart;
    let ilimit = iend as isize - 8;
    let prefix_start = ms.window.pos(ms.window.dict_limit);
    let cparams = ms.cparams;

    let sufficient_len = cparams.target_length.min(ZSTD_OPT_NUM as u32 - 1);
    let min_match = if cparams.min_match == 3 { 3 } else { 4 };
    let mut next_to_update3 = ms.next_to_update;

    // the tables, out of the match state while the match finder works on it
    let mut opt = std::mem::take(&mut ms.opt.price_table);
    let mut matches = std::mem::take(&mut ms.opt.match_table);
    let mut last_stretch = Optimal::default();

    // init
    rescale_freqs(&mut ms.opt, &src[istart..iend], opt_level);
    ip += usize::from(ip == prefix_start);

    let lit_price = |o: &OptState, p: usize| raw_literals_cost(&src[p..p + 1], o, opt_level) as i32;
    let ll_price = |o: &OptState, l: u32| lit_length_price(l, o, opt_level) as i32;
    let ll_incprice = |o: &OptState, l: u32| ll_price(o, l) - ll_price(o, l - 1);

    // Match Loop
    while (ip as isize) < ilimit {
        let mut cur: u32;
        let mut last_pos: u32;
        let shortest_path: bool;

        // find first match
        {
            let litlen = (ip - anchor) as u32;
            let ll0 = u32::from(litlen == 0);
            let nb_matches = bt_get_all_matches(
                &mut matches,
                ms,
                &mut next_to_update3,
                src,
                ip,
                iend,
                rep,
                ll0,
                min_match,
            );
            if nb_matches == 0 {
                ip += 1;
                continue;
            }

            // initialize opt[0]
            opt[0].mlen = 0; // there are only literals so far
            opt[0].litlen = litlen;
            // the literal length's price, whose variation depends on litlen
            opt[0].price = ll_price(&ms.opt, litlen);
            opt[0].rep = *rep;

            // large match -> immediate encoding
            let max_ml = matches[nb_matches as usize - 1].len;
            let max_off_base = matches[nb_matches as usize - 1].off;
            if max_ml > sufficient_len {
                last_stretch.litlen = 0;
                last_stretch.mlen = max_ml;
                last_stretch.off = max_off_base;
                cur = 0;
                last_pos = max_ml;
                shortest_path = true;
            } else {
                // set prices for first matches starting position == 0
                let mut pos = 1u32;
                while pos < min_match {
                    opt[pos as usize].price = ZSTD_MAX_PRICE;
                    opt[pos as usize].mlen = 0;
                    opt[pos as usize].litlen = litlen + pos;
                    pos += 1;
                }
                for m in &matches[..nb_matches as usize] {
                    let off_base = m.off;
                    let end = m.len;
                    while pos <= end {
                        let match_price = get_match_price(off_base, pos, &ms.opt, opt_level) as i32;
                        let sequence_price = opt[0].price.wrapping_add(match_price);
                        let o = &mut opt[pos as usize];
                        o.mlen = pos;
                        o.off = off_base;
                        o.litlen = 0; // end of match
                        o.price = sequence_price.wrapping_add(ll_price(&ms.opt, 0));
                        pos += 1;
                    }
                }
                last_pos = pos - 1;
                opt[pos as usize].price = ZSTD_MAX_PRICE;
                cur = 0;
                shortest_path = false;
            }
        }

        let mut to_shortest = shortest_path;
        if !to_shortest {
            // check further positions
            cur = 1;
            while cur <= last_pos {
                let c = cur as usize;
                let inr = ip + c;

                // Fix current position with one literal if cheaper
                {
                    let litlen = opt[c - 1].litlen + 1;
                    let price = opt[c - 1]
                        .price
                        .wrapping_add(lit_price(&ms.opt, ip + c - 1))
                        .wrapping_add(ll_incprice(&ms.opt, litlen));
                    if price <= opt[c].price {
                        let prev_match = opt[c];
                        opt[c] = opt[c - 1];
                        opt[c].litlen = litlen;
                        opt[c].price = price;
                        if opt_level >= 1 // additional check only for higher modes
                            && prev_match.litlen == 0 // replace a match
                            && ll_incprice(&ms.opt, 1) < 0 // ll1 is cheaper than ll0
                            && ip + c < iend
                        {
                            // check next position, in case it would be cheaper
                            let with1literal = prev_match
                                .price
                                .wrapping_add(lit_price(&ms.opt, ip + c))
                                .wrapping_add(ll_incprice(&ms.opt, 1));
                            let with_more_literals = price
                                .wrapping_add(lit_price(&ms.opt, ip + c))
                                .wrapping_add(ll_incprice(&ms.opt, litlen + 1));
                            if with1literal < with_more_literals && with1literal < opt[c + 1].price
                            {
                                // update offset history - before it disappears
                                let prev = (cur - prev_match.mlen) as usize;
                                let new_reps =
                                    new_rep(&opt[prev].rep, prev_match.off, opt[prev].litlen == 0);
                                opt[c + 1] = prev_match; // mlen & offbase
                                opt[c + 1].rep = new_reps;
                                opt[c + 1].litlen = 1;
                                opt[c + 1].price = with1literal;
                                if last_pos < cur + 1 {
                                    last_pos = cur + 1;
                                }
                            }
                        }
                    }
                }

                // Offset history is not updated during match comparison. Do it here, now that
                // the match is selected and confirmed.
                if opt[c].litlen == 0 {
                    // just finished a match => alter offset history
                    let prev = (cur - opt[c].mlen) as usize;
                    opt[c].rep = new_rep(&opt[prev].rep, opt[c].off, opt[prev].litlen == 0);
                }

                // last match must start at a minimum distance of 8 from oend
                if inr as isize > ilimit {
                    cur += 1;
                    continue;
                }

                if cur == last_pos {
                    break;
                }

                if opt_level == 0
                    && opt[c + 1].price <= opt[c].price + (BITCOST_MULTIPLIER / 2) as i32
                {
                    cur += 1;
                    continue; // skip unpromising positions; about ~+6% speed, -0.01 ratio
                }

                {
                    let ll0 = u32::from(opt[c].litlen == 0);
                    let previous_price = opt[c].price;
                    let base_price = previous_price.wrapping_add(ll_price(&ms.opt, 0));
                    let cur_rep = opt[c].rep;
                    let nb_matches = bt_get_all_matches(
                        &mut matches,
                        ms,
                        &mut next_to_update3,
                        src,
                        inr,
                        iend,
                        &cur_rep,
                        ll0,
                        min_match,
                    );

                    if nb_matches == 0 {
                        cur += 1;
                        continue;
                    }

                    let longest_ml = matches[nb_matches as usize - 1].len;
                    if (longest_ml > sufficient_len)
                        || (cur + longest_ml >= ZSTD_OPT_NUM as u32)
                        || (ip + c + longest_ml as usize >= iend)
                    {
                        last_stretch.mlen = longest_ml;
                        last_stretch.off = matches[nb_matches as usize - 1].off;
                        last_stretch.litlen = 0;
                        last_pos = cur + longest_ml;
                        to_shortest = true;
                        break;
                    }

                    // set prices using matches found at position == cur
                    for match_nb in 0..nb_matches as usize {
                        let offset = matches[match_nb].off;
                        let last_ml = matches[match_nb].len;
                        let start_ml = if match_nb > 0 {
                            matches[match_nb - 1].len + 1
                        } else {
                            min_match
                        };

                        let mut mlen = last_ml;
                        while mlen >= start_ml {
                            // scan downward
                            let pos = cur + mlen;
                            let price = base_price
                                .wrapping_add(
                                    get_match_price(offset, mlen, &ms.opt, opt_level) as i32
                                );

                            if (pos > last_pos) || (price < opt[pos as usize].price) {
                                while last_pos < pos {
                                    // fill empty positions, for future comparisons
                                    last_pos += 1;
                                    opt[last_pos as usize].price = ZSTD_MAX_PRICE;
                                    opt[last_pos as usize].litlen = 1; // just needs to be != 0, to mean "not an end of match"
                                }
                                let o = &mut opt[pos as usize];
                                o.mlen = mlen;
                                o.off = offset;
                                o.litlen = 0;
                                o.price = price;
                            } else if opt_level == 0 {
                                break; // early update abort; gets ~+10% speed for about -0.01 ratio loss
                            }
                            mlen -= 1;
                        }
                    }
                }
                opt[last_pos as usize + 1].price = ZSTD_MAX_PRICE;
                cur += 1;
            }

            if !to_shortest {
                last_stretch = opt[last_pos as usize];
                cur = last_pos - last_stretch.mlen;
            }
        }

        // _shortestPath: cur, last_pos, best_mlen, best_off have to be set
        if last_stretch.mlen == 0 {
            // no solution : all matches have been converted into literals
            ip += last_pos as usize;
            continue;
        }

        // Update offset history
        if last_stretch.litlen == 0 {
            // finishing on a match : update offset history
            *rep = new_rep(
                &opt[cur as usize].rep,
                last_stretch.off,
                opt[cur as usize].litlen == 0,
            );
        } else {
            *rep = last_stretch.rep;
            cur -= last_stretch.litlen;
        }

        // Let's write the shortest path solution. It is stored in @opt in reverse order,
        // starting from @storeEnd (==cur+2), effectively partially @opt overwriting. Content is
        // changed too: so far, @opt stored stretches (a match followed by literals); now it
        // stores sequences (literals followed by a match).
        {
            let store_end = cur as usize + 2;
            let mut store_start;
            let mut stretch_pos = cur as usize;

            if last_stretch.litlen > 0 {
                // last "sequence" is unfinished: just a bunch of literals
                opt[store_end].litlen = last_stretch.litlen;
                opt[store_end].mlen = 0;
                store_start = store_end - 1;
                opt[store_start] = last_stretch;
            }
            // upstream's next block is not the if's else: it always runs
            {
                opt[store_end] = last_stretch; // note: litlen will be fixed
                store_start = store_end;
            }
            loop {
                let next_stretch = opt[stretch_pos];
                opt[store_start].litlen = next_stretch.litlen;
                if next_stretch.mlen == 0 {
                    // reaching beginning of segment
                    break;
                }
                store_start -= 1;
                opt[store_start] = next_stretch; // note: litlen will be fixed
                stretch_pos -= (next_stretch.litlen + next_stretch.mlen) as usize;
            }

            // save sequences
            for store_pos in store_start..=store_end {
                let llen = opt[store_pos].litlen as usize;
                let mlen = opt[store_pos].mlen;
                let off_base = opt[store_pos].off;
                let advance = llen + mlen as usize;

                if mlen == 0 {
                    // only literals => must be last "sequence", actually starting a new
                    // stream of sequences
                    ip = anchor + llen; // last "sequence" is a bunch of literals => don't progress anchor
                    continue; // will finish
                }

                update_stats(&mut ms.opt, &src[anchor..anchor + llen], off_base, mlen);
                seq_store.store_seq(&src[anchor..anchor + llen], off_base, mlen as usize);
                anchor += advance;
                ip = anchor;
            }

            // update all costs
            set_base_prices(&mut ms.opt, opt_level);
        }
    }

    ms.opt.price_table = opt;
    ms.opt.match_table = matches;

    // Return the last literals size
    iend - anchor
}

/// Port of `ZSTD_initStats_ultra` (external/zstd/lib/compress/zstd_opt.c:1482-1507
/// @ zstd v1.5.7): a first pass over the frame's first block, only to seed the stats, then
/// forgotten: the window moves past it (upstream lowers `base` by the block's size, which
/// raises every index of the input by as much).
fn init_stats_ultra(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &[u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
) {
    let mut tmp_rep = *rep; // updated rep codes will sink here
    compress_block_opt_generic(ms, seq_store, &mut tmp_rep, src, istart, iend, 2); // generate stats into ms->opt

    // invalidate first scan from history, only keep entropy stats
    seq_store.reset();
    let src_size = (iend - istart) as u32;
    ms.window.base += src_size;
    ms.window.next_src += src_size;
    ms.window.dict_limit += src_size;
    ms.window.low_limit = ms.window.dict_limit;
    ms.next_to_update = ms.window.dict_limit;
}

/// Ports of `ZSTD_compressBlock_btopt`, `ZSTD_compressBlock_btultra` and
/// `ZSTD_compressBlock_btultra2` (external/zstd/lib/compress/zstd_opt.c:1463-1543 @ zstd v1.5.7):
/// the optimal parser at level 0 (btopt) or 2; btultra2 first seeds the stats with a pass over
/// the frame's first block.
pub(crate) fn compress_block_opt(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
    strategy: crate::zstd_compress_internal::Strategy,
) -> usize {
    use crate::zstd_compress_internal::{ZSTD_BTOPT, ZSTD_BTULTRA2};
    if strategy == ZSTD_BTOPT {
        return compress_block_opt_generic(ms, seq_store, rep, src, istart, iend, 0);
    }
    if strategy == ZSTD_BTULTRA2 {
        let curr = ms.window.idx(istart);
        // 2-passes strategy, only on a frame's first block without dictionary, prefix or ldm
        if (ms.opt.lit_length_sum == 0) // first block
            && seq_store.sequences.is_empty() // no ldm
            && (ms.window.dict_limit == ms.window.low_limit) // no dictionary
            && (curr == ms.window.dict_limit) // start of frame, nothing already loaded nor skipped
            && (iend - istart > ZSTD_PREDEF_THRESHOLD)
        {
            // input large enough to not employ default stats
            init_stats_ultra(ms, seq_store, rep, src, istart, iend);
        }
    }
    compress_block_opt_generic(ms, seq_store, rep, src, istart, iend, 2)
}
