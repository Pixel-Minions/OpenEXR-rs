// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_lazy.c` (external/zstd/lib/compress/zstd_lazy.c:1-2199 @ zstd v1.5.7): the
//! greedy, lazy and lazy2 parsers over the hash chain and the row match finder, and the
//! btlazy2 one over the binary tree (`ZSTD_DUBT_*`), without a dictionary; dictionaries and
//! extDict windows (which a one-shot compression never has) are n/a. Upstream's SSE2 tag comparison computes the
//! same bit mask as the scalar loop.

use crate::mem::highbit32;
use crate::zstd_compress_internal::{
    K_SEARCH_STRENGTH, MatchState, REPCODE1_TO_OFFBASE, SeqStore, ZSTD_ROW_HASH_CACHE_SIZE,
    get_lowest_match_index, get_lowest_prefix_index, hash_ptr, hash_ptr_salted, offbase_is_offset,
    offset_to_offbase, read32, zstd_count,
};

/// `kLazySkippingStep` (external/zstd/lib/compress/zstd_lazy.c:20 @ zstd v1.5.7).
const K_LAZY_SKIPPING_STEP: usize = 8;
/// `ZSTD_ROW_HASH_TAG_BITS` (external/zstd/lib/compress/zstd_lazy.h:24 @ zstd v1.5.7).
pub(crate) const ZSTD_ROW_HASH_TAG_BITS: u32 = 8;
/// `ZSTD_ROW_HASH_TAG_MASK` (external/zstd/lib/compress/zstd_lazy.c:779 @ zstd v1.5.7).
const ZSTD_ROW_HASH_TAG_MASK: u32 = (1 << ZSTD_ROW_HASH_TAG_BITS) - 1;
/// `ZSTD_ROW_HASH_MAX_ENTRIES` (external/zstd/lib/compress/zstd_lazy.c:780 @ zstd v1.5.7).
const ZSTD_ROW_HASH_MAX_ENTRIES: usize = 64;
/// `ZSTD_ROW_HASH_CACHE_MASK` (external/zstd/lib/compress/zstd_lazy.c:782 @ zstd v1.5.7).
const ZSTD_ROW_HASH_CACHE_MASK: u32 = ZSTD_ROW_HASH_CACHE_SIZE - 1;

/// Port of `searchMethod_e` (external/zstd/lib/compress/zstd_lazy.c:1424 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchMethod {
    HashChain,
    BinaryTree,
    RowHash,
}

/// `BOUNDED (4, v, 6)`.
#[inline]
pub(crate) fn bounded_4_6(v: u32) -> u32 {
    if v < 4 {
        4
    } else if v > 6 {
        6
    } else {
        v
    }
}

/// Port of `ZSTD_insertAndFindFirstIndex_internal`
/// (external/zstd/lib/compress/zstd_lazy.c:630-657 @ zstd v1.5.7): the chains updated up to `ip` (excluded), and the head of `ip`'s.
fn insert_and_find_first_index(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    mls: u32,
    lazy_skipping: bool,
) -> u32 {
    let hash_log = ms.cparams.hash_log;
    let chain_mask = (1u32 << ms.cparams.chain_log) - 1;
    let target = ms.window.idx(ip);
    let mut idx = ms.next_to_update;
    while idx < target {
        // catch up
        let h = hash_ptr(src, ms.window.pos(idx), hash_log, mls);
        ms.chain_table[(idx & chain_mask) as usize] = ms.hash_table[h];
        ms.hash_table[h] = idx;
        idx += 1;
        // Stop inserting every position when in the lazy skipping mode.
        if lazy_skipping {
            break;
        }
    }
    ms.next_to_update = target;
    ms.hash_table[hash_ptr(src, ip, hash_log, mls)]
}

/// Port of `ZSTD_HcFindBestMatch` (external/zstd/lib/compress/zstd_lazy.c:665-772
/// @ zstd v1.5.7), without a dictionary: the longest match along `ip`'s chain, its offBase in
/// `offset_ptr`.
fn hc_find_best_match(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    i_limit: usize,
    offset_ptr: &mut usize,
    mls: u32,
) -> usize {
    let chain_size = 1u32 << ms.cparams.chain_log;
    let chain_mask = chain_size - 1;
    let curr = ms.window.idx(ip);
    let max_distance = 1u32 << ms.cparams.window_log;
    let lowest_valid = ms.window.low_limit;
    let within_max_distance = if curr - lowest_valid > max_distance {
        curr - max_distance
    } else {
        lowest_valid
    };
    let low_limit = within_max_distance;
    // (`curr > chainSize ? curr - chainSize : 0`)
    let min_chain = curr.saturating_sub(chain_size);
    let mut nb_attempts = 1u32 << ms.cparams.search_log;
    let mut ml: usize = 4 - 1;

    // HC4 match finder
    let lazy_skipping = ms.lazy_skipping;
    let mut match_index = insert_and_find_first_index(ms, src, ip, mls, lazy_skipping);

    while (match_index >= low_limit) & (nb_attempts > 0) {
        let mut current_ml = 0;
        let m = ms.window.pos(match_index);
        // read 4B starting from (match + ml + 1 - sizeof(U32))
        if read32(src, m + ml - 3) == read32(src, ip + ml - 3) {
            // potentially better
            current_ml = zstd_count(src, ip, m, i_limit);
        }
        // save best solution
        if current_ml > ml {
            ml = current_ml;
            *offset_ptr = offset_to_offbase(curr - match_index) as usize;
            if ip + current_ml == i_limit {
                break; // best possible, avoids read overflow on next attempt
            }
        }
        if match_index <= min_chain {
            break;
        }
        match_index = ms.chain_table[(match_index & chain_mask) as usize];
        nb_attempts -= 1;
    }
    ml
}

/// Port of `ZSTD_row_nextIndex` (external/zstd/lib/compress/zstd_lazy.c:798-803 @ zstd v1.5.7):
/// the next position of a row, the head stepping back over `[1, entries)`.
#[inline]
fn row_next_index(tag_table: &mut [u8], tag_row: usize, row_mask: u32) -> u32 {
    let mut next = u32::from(tag_table[tag_row]).wrapping_sub(1) & row_mask;
    next += if next == 0 { row_mask } else { 0 }; // skip first position
    tag_table[tag_row] = next as u8;
    next
}

/// Port of `ZSTD_row_fillHashCache` (external/zstd/lib/compress/zstd_lazy.c:835-859
/// @ zstd v1.5.7): the hashes of up to 8 positions from `idx`, not beyond `i_limit`.
fn row_fill_hash_cache(ms: &mut MatchState, src: &[u8], mls: u32, mut idx: u32, i_limit: usize) {
    let hash_log = ms.row_hash_log;
    let p = ms.window.pos(idx);
    let max_elems_to_prefetch = if p > i_limit {
        0
    } else {
        (i_limit - p + 1) as u32
    };
    let lim = idx
        + if ZSTD_ROW_HASH_CACHE_SIZE < max_elems_to_prefetch {
            ZSTD_ROW_HASH_CACHE_SIZE
        } else {
            max_elems_to_prefetch
        };
    while idx < lim {
        let hash = hash_ptr_salted(
            src,
            ms.window.pos(idx),
            hash_log + ZSTD_ROW_HASH_TAG_BITS,
            mls,
            ms.hash_salt,
        ) as u32;
        ms.hash_cache[(idx & ZSTD_ROW_HASH_CACHE_MASK) as usize] = hash;
        idx += 1;
    }
}

/// Port of `ZSTD_row_nextCachedHash` (external/zstd/lib/compress/zstd_lazy.c:863-881
/// @ zstd v1.5.7): `idx`'s hash from the cache, which takes `idx + 8`'s.
#[inline]
fn row_next_cached_hash(ms: &mut MatchState, src: &[u8], idx: u32, mls: u32) -> u32 {
    let new_hash = hash_ptr_salted(
        src,
        ms.window.pos(idx + ZSTD_ROW_HASH_CACHE_SIZE),
        ms.row_hash_log + ZSTD_ROW_HASH_TAG_BITS,
        mls,
        ms.hash_salt,
    ) as u32;
    let slot = (idx & ZSTD_ROW_HASH_CACHE_MASK) as usize;
    let hash = ms.hash_cache[slot];
    ms.hash_cache[slot] = new_hash;
    hash
}

/// Port of `ZSTD_row_update_internalImpl` (external/zstd/lib/compress/zstd_lazy.c:883-910
/// @ zstd v1.5.7): positions `[start, end)` inserted in their rows.
#[allow(clippy::too_many_arguments)]
fn row_update_internal_impl(
    ms: &mut MatchState,
    src: &[u8],
    mut update_start_idx: u32,
    update_end_idx: u32,
    mls: u32,
    row_log: u32,
    row_mask: u32,
    use_cache: bool,
) {
    while update_start_idx < update_end_idx {
        let hash = if use_cache {
            row_next_cached_hash(ms, src, update_start_idx, mls)
        } else {
            hash_ptr_salted(
                src,
                ms.window.pos(update_start_idx),
                ms.row_hash_log + ZSTD_ROW_HASH_TAG_BITS,
                mls,
                ms.hash_salt,
            ) as u32
        };
        let rel_row = ((hash >> ZSTD_ROW_HASH_TAG_BITS) << row_log) as usize;
        let pos = row_next_index(&mut ms.tag_table, rel_row, row_mask) as usize;
        ms.tag_table[rel_row + pos] = (hash & ZSTD_ROW_HASH_TAG_MASK) as u8;
        ms.hash_table[rel_row + pos] = update_start_idx;
        update_start_idx += 1;
    }
}

/// Port of `ZSTD_row_update_internal` (external/zstd/lib/compress/zstd_lazy.c:914-942
/// @ zstd v1.5.7): the rows updated up to `ip`, skipping the middle of long matches.
fn row_update_internal(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    mls: u32,
    row_log: u32,
    row_mask: u32,
    use_cache: bool,
) {
    let mut idx = ms.next_to_update;
    let target = ms.window.idx(ip);
    const K_SKIP_THRESHOLD: u32 = 384;
    const K_MAX_MATCH_START_POSITIONS_TO_UPDATE: u32 = 96;
    const K_MAX_MATCH_END_POSITIONS_TO_UPDATE: u32 = 32;

    if use_cache {
        // Only skip positions when using hash cache, i.e. if we are loading a dict, don't
        // skip anything. If we decide to skip, then we only update a set number of positions
        // at the beginning and end of the match.
        if target - idx > K_SKIP_THRESHOLD {
            let bound = idx + K_MAX_MATCH_START_POSITIONS_TO_UPDATE;
            row_update_internal_impl(ms, src, idx, bound, mls, row_log, row_mask, use_cache);
            idx = target - K_MAX_MATCH_END_POSITIONS_TO_UPDATE;
            row_fill_hash_cache(ms, src, mls, idx, ip + 1);
        }
    }
    row_update_internal_impl(ms, src, idx, target, mls, row_log, row_mask, use_cache);
    ms.next_to_update = target;
}

/// Port of `ZSTD_row_getMatchMask` (external/zstd/lib/compress/zstd_lazy.c:1059-1123
/// @ zstd v1.5.7), with `ZSTD_row_getSSEMask` (lines 986-1004): bit `n` set when the row's
/// `n`th entry after the head has the tag (the x86-64 group width is 1). The mask is computed
/// as the function's SWAR branch computes it (lines 1080-1101, little-endian), eight tags a
/// word: the same mask as the SSE2 version's, whose group width is 1 too.
#[inline]
fn row_get_match_mask(tag_row: &[u8], tag: u8, head_grouped: u32, row_entries: u32) -> u64 {
    const CHUNK: usize = 8;
    const SHIFT: u32 = (CHUNK * 8 - CHUNK) as u32;
    const X01: u64 = u64::MAX / 0xFF;
    const X80: u64 = X01 << 7;
    const EXTRACT_MAGIC: u64 = (u64::MAX / 0x7F) >> CHUNK;
    let splat = u64::from(tag).wrapping_mul(X01);
    let mut matches: u64 = 0;
    let mut i = row_entries as usize - CHUNK;
    loop {
        let mut chunk = u64::from_le_bytes(tag_row[i..i + CHUNK].try_into().unwrap());
        chunk ^= splat;
        chunk = (((chunk | X80).wrapping_sub(X01)) | chunk) & X80;
        matches <<= CHUNK;
        matches |= chunk.wrapping_mul(EXTRACT_MAGIC) >> SHIFT;
        if i < CHUNK {
            break;
        }
        i -= CHUNK;
    }
    matches = !matches;
    match row_entries {
        16 => u64::from((matches as u16).rotate_right(head_grouped)),
        32 => u64::from((matches as u32).rotate_right(head_grouped)),
        _ => matches.rotate_right(head_grouped),
    }
}

/// Port of `ZSTD_RowFindBestMatch` (external/zstd/lib/compress/zstd_lazy.c:1139-1352
/// @ zstd v1.5.7), without a dictionary: the longest match among the row's entries with
/// `ip`'s tag, its offBase in `offset_ptr`.
fn row_find_best_match(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    i_limit: usize,
    offset_ptr: &mut usize,
    mls: u32,
    row_log: u32,
) -> usize {
    let curr = ms.window.idx(ip);
    let max_distance = 1u32 << ms.cparams.window_log;
    let lowest_valid = ms.window.low_limit;
    let within_max_distance = if curr - lowest_valid > max_distance {
        curr - max_distance
    } else {
        lowest_valid
    };
    let low_limit = within_max_distance;
    let row_entries = 1u32 << row_log;
    let row_mask = row_entries - 1;
    // nb of searches is capped at nb entries per row
    let capped_search_log = if ms.cparams.search_log < row_log {
        ms.cparams.search_log
    } else {
        row_log
    };
    let group_width = 1;
    let hash_salt = ms.hash_salt;
    let mut nb_attempts = 1u32 << capped_search_log;
    let mut ml: usize = 4 - 1;

    // Update the hashTable and tagTable up to (but not including) ip
    let hash = if !ms.lazy_skipping {
        row_update_internal(ms, src, ip, mls, row_log, row_mask, true);
        row_next_cached_hash(ms, src, curr, mls)
    } else {
        // Stop inserting every position when in the lazy skipping mode. The hash cache is
        // also not kept up to date in this mode.
        let h = hash_ptr_salted(
            src,
            ip,
            ms.row_hash_log + ZSTD_ROW_HASH_TAG_BITS,
            mls,
            hash_salt,
        ) as u32;
        ms.next_to_update = curr;
        h
    };
    ms.hash_salt_entropy = ms.hash_salt_entropy.wrapping_add(hash); // collect salt entropy

    // Get the hash for ip, compute the appropriate row
    let rel_row = ((hash >> ZSTD_ROW_HASH_TAG_BITS) << row_log) as usize;
    let tag = hash & ZSTD_ROW_HASH_TAG_MASK;
    let head_grouped = (u32::from(ms.tag_table[rel_row]) & row_mask) * group_width;
    let mut match_buffer = [0u32; ZSTD_ROW_HASH_MAX_ENTRIES];
    let mut num_matches = 0;
    let mut matches = row_get_match_mask(
        &ms.tag_table[rel_row..],
        tag as u8,
        head_grouped,
        row_entries,
    );

    // Cycle through the matches
    while (matches > 0) && (nb_attempts > 0) {
        let match_pos = ((head_grouped + matches.trailing_zeros()) / group_width) & row_mask;
        let match_index = ms.hash_table[rel_row + match_pos as usize];
        matches &= matches - 1;
        if match_pos == 0 {
            continue;
        }
        if match_index < low_limit {
            break;
        }
        match_buffer[num_matches] = match_index;
        num_matches += 1;
        nb_attempts -= 1;
    }

    // Speed opt: insert current byte into hashtable too. This allows us to avoid one
    // iteration of the loop in ZSTD_row_update_internal() at the next search.
    {
        let pos = row_next_index(&mut ms.tag_table, rel_row, row_mask) as usize;
        ms.tag_table[rel_row + pos] = tag as u8;
        ms.hash_table[rel_row + pos] = ms.next_to_update;
        ms.next_to_update += 1;
    }

    // Return the longest match
    for &match_index in &match_buffer[..num_matches] {
        let mut current_ml = 0;
        let m = ms.window.pos(match_index);
        // read 4B starting from (match + ml + 1 - sizeof(U32))
        if read32(src, m + ml - 3) == read32(src, ip + ml - 3) {
            // potentially better
            current_ml = zstd_count(src, ip, m, i_limit);
        }
        // Save best solution
        if current_ml > ml {
            ml = current_ml;
            *offset_ptr = offset_to_offbase(curr - match_index) as usize;
            if ip + current_ml == i_limit {
                break; // best possible, avoids read overflow on next attempt
            }
        }
    }
    ml
}

/// `ZSTD_DUBT_UNSORTED_MARK` (external/zstd/lib/compress/zstd_compress_internal.h:34
/// @ zstd v1.5.7): index 1 means "unsorted".
const ZSTD_DUBT_UNSORTED_MARK: u32 = 1;

/// A tree pointer of the binary tree search: a slot of the tree (`bt`), or upstream's
/// `dummy32`, where writes go to be forgotten.
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

/// Port of `ZSTD_updateDUBT` (external/zstd/lib/compress/zstd_lazy.c:27-64 @ zstd v1.5.7):
/// the positions from `nextToUpdate` to `ip` inserted in the hash table and, unsorted, in the
/// binary tree (as a chain).
fn update_dubt(ms: &mut MatchState, src: &[u8], ip: usize, mls: u32) {
    let hash_log = ms.cparams.hash_log;
    let bt_log = ms.cparams.chain_log - 1;
    let bt_mask = (1u32 << bt_log) - 1;
    let target = ms.window.idx(ip);
    let mut idx = ms.next_to_update;

    while idx < target {
        let h = hash_ptr(src, ms.window.pos(idx), hash_log, mls); // assumption : ip + 8 <= iend
        let match_index = ms.hash_table[h];
        let next_candidate = 2 * (idx & bt_mask) as usize;

        ms.hash_table[h] = idx; // Update Hash Table
        ms.chain_table[next_candidate] = match_index; // update BT like a chain
        ms.chain_table[next_candidate + 1] = ZSTD_DUBT_UNSORTED_MARK;
        idx += 1;
    }
    ms.next_to_update = target;
}

/// Port of `ZSTD_insertDUBT1` (external/zstd/lib/compress/zstd_lazy.c:67-157 @ zstd v1.5.7),
/// without a dictionary: one already inserted but unsorted position, `curr`, sorted into the
/// tree.
fn insert_dubt1(
    ms: &mut MatchState,
    src: &[u8],
    curr: u32,
    input_end: usize,
    mut nb_compares: u32,
    bt_low: u32,
) {
    let bt_log = ms.cparams.chain_log - 1;
    let bt_mask = (1u32 << bt_log) - 1;
    let mut common_length_smaller = 0usize;
    let mut common_length_larger = 0usize;
    let ip = ms.window.pos(curr);
    let iend = input_end;
    let mut smaller_ptr = BtSlot::At(2 * (curr & bt_mask) as usize);
    let mut larger_ptr = BtSlot::At(2 * (curr & bt_mask) as usize + 1);
    // this candidate is unsorted : next sorted candidate is reached through *smallerPtr, while
    // *largerPtr contains previous unsorted candidate (which is already saved and can be
    // overwritten)
    let mut match_index = ms.chain_table[2 * (curr & bt_mask) as usize];
    let window_valid = ms.window.low_limit;
    let max_distance = 1u32 << ms.cparams.window_log;
    let window_low = if curr - window_valid > max_distance {
        curr - max_distance
    } else {
        window_valid
    };

    while nb_compares != 0 && match_index > window_low {
        let next_ptr = 2 * (match_index & bt_mask) as usize;
        // guaranteed minimum nb of common bytes
        let mut match_length = common_length_smaller.min(common_length_larger);
        let m = ms.window.pos(match_index);
        match_length += zstd_count(src, ip + match_length, m + match_length, iend);

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
}

/// Port of `ZSTD_DUBT_findBestMatch` (external/zstd/lib/compress/zstd_lazy.c:241-390
/// @ zstd v1.5.7), without a dictionary: the unsorted candidates sorted in, then the longest
/// match searched in the tree, `ip` inserted; the length, with the offset in `off_base_ptr`.
fn dubt_find_best_match(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    iend: usize,
    off_base_ptr: &mut usize,
    mls: u32,
) -> usize {
    let hash_log = ms.cparams.hash_log;
    let h = hash_ptr(src, ip, hash_log, mls);
    let mut match_index = ms.hash_table[h];

    let curr = ms.window.idx(ip);
    let window_low = get_lowest_match_index(ms, curr, ms.cparams.window_log);

    let bt_log = ms.cparams.chain_log - 1;
    let bt_mask = (1u32 << bt_log) - 1;
    let bt_low = curr.saturating_sub(bt_mask); // (btMask >= curr) ? 0 : curr - btMask
    let unsort_limit = bt_low.max(window_low);

    let mut next_candidate = 2 * (match_index & bt_mask) as usize;
    let mut unsorted_mark = next_candidate + 1;
    let mut nb_compares = 1u32 << ms.cparams.search_log;
    let mut nb_candidates = nb_compares;
    let mut previous_candidate = 0u32;

    // reach end of unsorted candidates list
    while (match_index > unsort_limit)
        && (ms.chain_table[unsorted_mark] == ZSTD_DUBT_UNSORTED_MARK)
        && (nb_candidates > 1)
    {
        // the unsortedMark becomes a reversed chain, to move up back to original position
        ms.chain_table[unsorted_mark] = previous_candidate;
        previous_candidate = match_index;
        match_index = ms.chain_table[next_candidate];
        next_candidate = 2 * (match_index & bt_mask) as usize;
        unsorted_mark = next_candidate + 1;
        nb_candidates -= 1;
    }

    // nullify last candidate if it's still unsorted (simplification, detrimental to
    // compression ratio, beneficial for speed)
    if (match_index > unsort_limit) && (ms.chain_table[unsorted_mark] == ZSTD_DUBT_UNSORTED_MARK) {
        ms.chain_table[next_candidate] = 0;
        ms.chain_table[unsorted_mark] = 0;
    }

    // batch sort stacked candidates
    match_index = previous_candidate;
    while match_index != 0 {
        // will end on matchIndex == 0
        let next_candidate_idx = ms.chain_table[2 * (match_index & bt_mask) as usize + 1];
        insert_dubt1(ms, src, match_index, iend, nb_candidates, unsort_limit);
        match_index = next_candidate_idx;
        nb_candidates += 1;
    }

    // find longest match
    let mut common_length_smaller = 0usize;
    let mut common_length_larger = 0usize;
    let mut smaller_ptr = BtSlot::At(2 * (curr & bt_mask) as usize);
    let mut larger_ptr = BtSlot::At(2 * (curr & bt_mask) as usize + 1);
    let mut match_end_idx = curr + 8 + 1;
    let mut best_length = 0usize;

    match_index = ms.hash_table[h];
    ms.hash_table[h] = curr; // Update Hash Table

    while nb_compares != 0 && match_index > window_low {
        let next_ptr = 2 * (match_index & bt_mask) as usize;
        // guaranteed minimum nb of common bytes
        let mut match_length = common_length_smaller.min(common_length_larger);
        let m = ms.window.pos(match_index);
        match_length += zstd_count(src, ip + match_length, m + match_length, iend);

        if match_length > best_length {
            if match_length > (match_end_idx - match_index) as usize {
                match_end_idx = match_index + match_length as u32;
            }
            // (4*(int)(matchLength-bestLength)) > (int)(ZSTD_highbit32(curr - matchIndex + 1)
            // - ZSTD_highbit32((U32)*offBasePtr)), the subtraction in U32
            if 4 * as_int(match_length - best_length)
                > highbit32(curr - match_index + 1).wrapping_sub(highbit32(*off_base_ptr as u32))
                    as i32
            {
                best_length = match_length;
                *off_base_ptr = offset_to_offbase(curr - match_index) as usize;
            }
            if ip + match_length == iend {
                // equal : no way to know if inf or sup; drop, to guarantee consistency
                break;
            }
        }

        if src[m + match_length] < src[ip + match_length] {
            // match is smaller than current
            bt_set(&mut ms.chain_table, smaller_ptr, match_index); // update smaller idx
            common_length_smaller = match_length; // all smaller will now have at least this guaranteed common length
            if match_index <= bt_low {
                // beyond tree size, stop the search
                smaller_ptr = BtSlot::Dummy;
                break;
            }
            smaller_ptr = BtSlot::At(next_ptr + 1); // new "smaller" => larger of match
            match_index = ms.chain_table[next_ptr + 1]; // new matchIndex larger than previous (closer to current)
        } else {
            // match is larger than current
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
    best_length
}

/// Port of `ZSTD_BtFindBestMatch` (external/zstd/lib/compress/zstd_lazy.c:392-404
/// @ zstd v1.5.7): the tree updated to `ip`, then the best match there.
fn bt_find_best_match(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    i_limit: usize,
    off_base_ptr: &mut usize,
    mls: u32,
) -> usize {
    if ip < ms.window.pos(ms.next_to_update) {
        return 0; // skipped area
    }
    update_dubt(ms, src, ip, mls);
    dubt_find_best_match(ms, src, ip, i_limit, off_base_ptr, mls)
}

/// Port of `ZSTD_searchMax` (external/zstd/lib/compress/zstd_lazy.c:1487-1508 @ zstd v1.5.7),
/// without a dictionary.
#[inline]
#[allow(clippy::too_many_arguments)]
fn search_max(
    ms: &mut MatchState,
    src: &[u8],
    ip: usize,
    iend: usize,
    offset_ptr: &mut usize,
    mls: u32,
    row_log: u32,
    search_method: SearchMethod,
) -> usize {
    match search_method {
        SearchMethod::HashChain => hc_find_best_match(ms, src, ip, iend, offset_ptr, mls),
        SearchMethod::BinaryTree => bt_find_best_match(ms, src, ip, iend, offset_ptr, mls),
        SearchMethod::RowHash => row_find_best_match(ms, src, ip, iend, offset_ptr, mls, row_log),
    }
}

/// `(int) (x)` of a `size_t` expression.
#[inline]
fn as_int(x: usize) -> i32 {
    x as i32
}

/// Port of `ZSTD_compressBlock_lazy_generic` (external/zstd/lib/compress/zstd_lazy.c:1514-1778
/// @ zstd v1.5.7), without a dictionary: the block `src[istart..iend]` parsed into
/// `seq_store`, searching `depth` positions ahead (0 greedy, 1 lazy, 2 lazy2); returns the
/// last literals' size.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compress_block_lazy_generic(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
    search_method: SearchMethod,
    depth: u32,
) -> usize {
    let mut ip = istart;
    let mut anchor = istart;
    let ilimit = if search_method == SearchMethod::RowHash {
        iend as isize - 8 - ZSTD_ROW_HASH_CACHE_SIZE as isize
    } else {
        iend as isize - 8
    };
    let prefix_lowest_index = ms.window.dict_limit;
    let prefix_lowest = ms.window.pos(prefix_lowest_index);
    let mls = bounded_4_6(ms.cparams.min_match);
    let row_log = bounded_4_6(ms.cparams.search_log);
    let mut offset_1 = rep[0];
    let mut offset_2 = rep[1];
    let mut offset_saved1 = 0;
    let mut offset_saved2 = 0;

    let dict_and_prefix_length = (ip - prefix_lowest) as u32;
    ip += usize::from(dict_and_prefix_length == 0);
    {
        let curr = ms.window.idx(ip);
        let window_low = get_lowest_prefix_index(ms, curr, ms.cparams.window_log);
        let max_rep = curr - window_low;
        if offset_2 > max_rep {
            offset_saved2 = offset_2;
            offset_2 = 0;
        }
        if offset_1 > max_rep {
            offset_saved1 = offset_1;
            offset_1 = 0;
        }
    }

    // Reset the lazy skipping state
    ms.lazy_skipping = false;

    // (`ilimit` before the input fills nothing: upstream's pointer is below `base + idx`.)
    if search_method == SearchMethod::RowHash && ilimit >= 0 {
        row_fill_hash_cache(ms, src, mls, ms.next_to_update, ilimit as usize);
    }

    // Match Loop
    while (ip as isize) < ilimit {
        let ilimit = ilimit as usize;
        let mut match_length: usize = 0;
        let mut off_base: usize = REPCODE1_TO_OFFBASE as usize;
        let mut start = ip + 1;
        let mut store = false;

        // check repCode
        if (offset_1 > 0) & (read32(src, ip + 1 - offset_1 as usize) == read32(src, ip + 1)) {
            match_length = zstd_count(src, ip + 1 + 4, ip + 1 + 4 - offset_1 as usize, iend) + 4;
            if depth == 0 {
                store = true;
            }
        }

        if !store {
            // first search (depth 0)
            {
                let mut offbase_found: usize = 999999999;
                let ml2 = search_max(
                    ms,
                    src,
                    ip,
                    iend,
                    &mut offbase_found,
                    mls,
                    row_log,
                    search_method,
                );
                if ml2 > match_length {
                    match_length = ml2;
                    start = ip;
                    off_base = offbase_found;
                }
            }

            if match_length < 4 {
                // jump faster over incompressible sections
                let step = ((ip - anchor) >> K_SEARCH_STRENGTH) + 1;
                ip += step;
                // Enter the lazy skipping mode once we are skipping more than 8 bytes at a
                // time. In this mode we stop inserting every position into our tables, and
                // only insert positions that we search, which is one in step positions.
                ms.lazy_skipping = step > K_LAZY_SKIPPING_STEP;
                continue;
            }

            // let's try to find a better solution
            if depth >= 1 {
                while ip < ilimit {
                    ip += 1;
                    if (off_base != 0)
                        && ((offset_1 > 0)
                            & (read32(src, ip) == read32(src, ip - offset_1 as usize)))
                    {
                        let ml_rep = zstd_count(src, ip + 4, ip + 4 - offset_1 as usize, iend) + 4;
                        let gain2 = as_int(ml_rep * 3);
                        let gain1 = as_int(
                            (match_length * 3)
                                .wrapping_sub(highbit32(off_base as u32) as usize)
                                .wrapping_add(1),
                        );
                        if (ml_rep >= 4) && (gain2 > gain1) {
                            match_length = ml_rep;
                            off_base = REPCODE1_TO_OFFBASE as usize;
                            start = ip;
                        }
                    }
                    {
                        let mut ofb_candidate: usize = 999999999;
                        let ml2 = search_max(
                            ms,
                            src,
                            ip,
                            iend,
                            &mut ofb_candidate,
                            mls,
                            row_log,
                            search_method,
                        );
                        // raw approx
                        let gain2 = as_int(
                            (ml2 * 4).wrapping_sub(highbit32(ofb_candidate as u32) as usize),
                        );
                        let gain1 = as_int(
                            (match_length * 4)
                                .wrapping_sub(highbit32(off_base as u32) as usize)
                                .wrapping_add(4),
                        );
                        if (ml2 >= 4) && (gain2 > gain1) {
                            match_length = ml2;
                            off_base = ofb_candidate;
                            start = ip;
                            continue; // search a better one
                        }
                    }

                    // let's find an even better one
                    if (depth == 2) && (ip < ilimit) {
                        ip += 1;
                        if (off_base != 0)
                            && ((offset_1 > 0)
                                & (read32(src, ip) == read32(src, ip - offset_1 as usize)))
                        {
                            let ml_rep =
                                zstd_count(src, ip + 4, ip + 4 - offset_1 as usize, iend) + 4;
                            let gain2 = as_int(ml_rep * 4);
                            let gain1 = as_int(
                                (match_length * 4)
                                    .wrapping_sub(highbit32(off_base as u32) as usize)
                                    .wrapping_add(1),
                            );
                            if (ml_rep >= 4) && (gain2 > gain1) {
                                match_length = ml_rep;
                                off_base = REPCODE1_TO_OFFBASE as usize;
                                start = ip;
                            }
                        }
                        {
                            let mut ofb_candidate: usize = 999999999;
                            let ml2 = search_max(
                                ms,
                                src,
                                ip,
                                iend,
                                &mut ofb_candidate,
                                mls,
                                row_log,
                                search_method,
                            );
                            // raw approx
                            let gain2 = as_int(
                                (ml2 * 4).wrapping_sub(highbit32(ofb_candidate as u32) as usize),
                            );
                            let gain1 = as_int(
                                (match_length * 4)
                                    .wrapping_sub(highbit32(off_base as u32) as usize)
                                    .wrapping_add(7),
                            );
                            if (ml2 >= 4) && (gain2 > gain1) {
                                match_length = ml2;
                                off_base = ofb_candidate;
                                start = ip;
                                continue;
                            }
                        }
                    }
                    break; // nothing found : store previous solution
                }
            }

            // catch up
            if offbase_is_offset(off_base as u32) {
                let off = off_base - 3;
                while ((start > anchor) & (start as isize - off as isize > prefix_lowest as isize))
                    && (src[start - 1] == src[start - off - 1])
                {
                    // only search for offset within prefix
                    start -= 1;
                    match_length += 1;
                }
                offset_2 = offset_1;
                offset_1 = off as u32;
            }
        }

        // store sequence
        {
            seq_store.store_seq(&src[anchor..start], off_base as u32, match_length);
            ip = start + match_length;
            anchor = ip;
        }
        if ms.lazy_skipping {
            // We've found a match, disable lazy skipping mode, and refill the hash cache.
            if search_method == SearchMethod::RowHash {
                row_fill_hash_cache(ms, src, mls, ms.next_to_update, ilimit);
            }
            ms.lazy_skipping = false;
        }

        // check immediate repcode
        while ((ip <= ilimit) & (offset_2 > 0))
            && (read32(src, ip) == read32(src, ip - offset_2 as usize))
        {
            // store sequence
            match_length = zstd_count(src, ip + 4, ip + 4 - offset_2 as usize, iend) + 4;
            // swap repcodes
            std::mem::swap(&mut offset_1, &mut offset_2);
            seq_store.store_seq(&[], REPCODE1_TO_OFFBASE, match_length);
            ip += match_length;
            anchor = ip;
        }
    }

    // If offset_1 started invalid (offsetSaved1 != 0) and became valid (offset_1 != 0),
    // rotate saved offsets.
    offset_saved2 = if (offset_saved1 != 0) && (offset_1 != 0) {
        offset_saved1
    } else {
        offset_saved2
    };

    // save reps for next block
    rep[0] = if offset_1 != 0 {
        offset_1
    } else {
        offset_saved1
    };
    rep[1] = if offset_2 != 0 {
        offset_2
    } else {
        offset_saved2
    };

    // Return the last literals size
    iend - anchor
}

/// Ports of `ZSTD_compressBlock_greedy`, `_greedy_row`, `_lazy`, `_lazy_row`, `_lazy2`,
/// `_lazy2_row` and `_btlazy2` (external/zstd/lib/compress/zstd_lazy.c:1784-1898 @ zstd v1.5.7):
/// the lazy parser of `depth` over `search_method`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn compress_block_lazy(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
    search_method: SearchMethod,
    depth: u32,
) -> usize {
    compress_block_lazy_generic(ms, seq_store, rep, src, istart, iend, search_method, depth)
}
