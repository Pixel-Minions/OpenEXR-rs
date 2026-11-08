// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_fast.c` (external/zstd/lib/compress/zstd_fast.c:1-985 @ zstd v1.5.7): the
//! `fast` strategy's block compressor, without a dictionary (`ZSTD_compressCCtx` uses no
//! other). Its two variants, the conditional-move one (`useCmov`, for windows under 2^19) and
//! the branching one, find the same matches: a candidate below the prefix never matches in
//! either; one port serves both. The dictionary and external-dictionary variants and the
//! table fill for dictionaries are n/a.

use crate::zstd_compress_internal::{
    HASH_READ_SIZE, K_SEARCH_STRENGTH, MatchState, REPCODE1_TO_OFFBASE, SeqStore,
    get_lowest_prefix_index, hash_ptr, offset_to_offbase, read32, zstd_count,
};

/// Port of `ZSTD_match4Found_branch` and `ZSTD_match4Found_cmov`
/// (external/zstd/lib/compress/zstd_fast.c:102-139 @ zstd v1.5.7): whether the 4 bytes at
/// `ip` are those at index `match_idx`, an index below `idx_low_limit` never matching.
#[inline]
fn match4_found(
    ms: &MatchState,
    src: &[u8],
    ip: usize,
    match_idx: u32,
    idx_low_limit: u32,
) -> bool {
    match_idx >= idx_low_limit && read32(src, ip) == read32(src, ms.window.pos(match_idx))
}

/// Where the inner search loop of [`compress_block_fast_no_dict_generic`] left: upstream's
/// labels `_cleanup`, `_offset` and `_match` (a repcode match).
enum Found {
    Cleanup,
    Offset {
        ip0: usize,
        match_idx: u32,
        current0: u32,
    },
    Match {
        ip0: usize,
        match0: usize,
        m_length: usize,
        current0: u32,
    },
}

/// Port of `ZSTD_compressBlock_fast_noDict_generic`
/// (external/zstd/lib/compress/zstd_fast.c:190-423 @ zstd v1.5.7): the block
/// `src[istart..iend]` parsed into `seq_store` with matches of `mls` bytes or more; returns
/// the last literals' size.
#[allow(clippy::too_many_arguments)]
fn compress_block_fast_no_dict_generic(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
    mls: u32,
) -> usize {
    let cparams = ms.cparams;
    let window = ms.window;
    let hlog = cparams.hash_log;
    let step_size = cparams.target_length as usize + usize::from(cparams.target_length == 0) + 1; // min 2
    let end_index = window.idx(iend);
    let prefix_start_index = get_lowest_prefix_index(ms, end_index, cparams.window_log);
    let prefix_start = window.pos(prefix_start_index);
    let ilimit = iend as isize - HASH_READ_SIZE as isize;

    let mut anchor = istart;
    let mut ip0 = istart;

    let mut rep_offset1 = rep[0];
    let mut rep_offset2 = rep[1];
    let mut offset_saved1 = 0u32;
    let mut offset_saved2 = 0u32;

    let k_step_incr = 1usize << (K_SEARCH_STRENGTH - 1);

    ip0 += usize::from(ip0 == prefix_start);
    {
        let curr = window.idx(ip0);
        let window_low = get_lowest_prefix_index(ms, curr, cparams.window_log);
        let max_rep = curr - window_low;
        if rep_offset2 > max_rep {
            offset_saved2 = rep_offset2;
            rep_offset2 = 0;
        }
        if rep_offset1 > max_rep {
            offset_saved1 = rep_offset1;
            rep_offset1 = 0;
        }
    }

    // start each op
    loop {
        // _start: Requires: ip0
        let mut step = step_size;
        let mut next_step = ip0 + k_step_incr;

        // calculate positions, ip0 - anchor == 0, so we skip step calc
        let mut ip1 = ip0 + 1;
        let mut ip2 = ip0 + step;
        let mut ip3 = ip2 + 1;

        let found = if ip3 as isize >= ilimit {
            Found::Cleanup
        } else {
            let mut hash0 = hash_ptr(src, ip0, hlog, mls);
            let mut hash1 = hash_ptr(src, ip1, hlog, mls);

            let mut match_idx = ms.hash_table[hash0];

            loop {
                // load repcode match for ip[2]
                let rval = read32(src, ip2 - rep_offset1 as usize);

                // write back hash table entry
                let current0 = window.idx(ip0);
                ms.hash_table[hash0] = current0;

                // check repcode at ip[2]
                if (read32(src, ip2) == rval) & (rep_offset1 > 0) {
                    let mut ip = ip2;
                    let mut match0 = ip - rep_offset1 as usize;
                    let m_length = usize::from(src[ip - 1] == src[match0 - 1]);
                    ip -= m_length;
                    match0 -= m_length;

                    // Write next hash table entry: it's already calculated. This write is
                    // known to be safe because ip1 is before the repcode (ip2).
                    ms.hash_table[hash1] = window.idx(ip1);

                    break Found::Match {
                        ip0: ip,
                        match0,
                        m_length: m_length + 4,
                        current0,
                    };
                }

                if match4_found(ms, src, ip0, match_idx, prefix_start_index) {
                    // Write next hash table entry (it's already calculated). This write is
                    // known to be safe because the ip1 == ip0 + 1, so searching will resume
                    // after ip1
                    ms.hash_table[hash1] = window.idx(ip1);

                    break Found::Offset {
                        ip0,
                        match_idx,
                        current0,
                    };
                }

                // lookup ip[1]
                match_idx = ms.hash_table[hash1];

                // hash ip[2]
                hash0 = hash1;
                hash1 = hash_ptr(src, ip2, hlog, mls);

                // advance to next positions
                ip0 = ip1;
                ip1 = ip2;
                ip2 = ip3;

                // write back hash table entry
                let current0 = window.idx(ip0);
                ms.hash_table[hash0] = current0;

                if match4_found(ms, src, ip0, match_idx, prefix_start_index) {
                    // Write next hash table entry, since it's already calculated
                    if step <= 4 {
                        // Avoid writing an index if it's >= position where search will
                        // resume. The minimum possible match has length 4, so search can
                        // resume at ip0 + 4.
                        ms.hash_table[hash1] = window.idx(ip1);
                    }
                    break Found::Offset {
                        ip0,
                        match_idx,
                        current0,
                    };
                }

                // lookup ip[1]
                match_idx = ms.hash_table[hash1];

                // hash ip[2]
                hash0 = hash1;
                hash1 = hash_ptr(src, ip2, hlog, mls);

                // advance to next positions
                ip0 = ip1;
                ip1 = ip2;
                ip2 = ip0 + step;
                ip3 = ip1 + step;

                // calculate step
                if ip2 >= next_step {
                    step += 1;
                    next_step += k_step_incr;
                }
                if ip3 as isize >= ilimit {
                    break Found::Cleanup;
                }
            }
        };

        let (ip, match0, m_length, off_base, current0) = match found {
            Found::Cleanup => {
                // When the repcodes are outside of the prefix, we set them to zero before the
                // loop; restore them after the block (see upstream's note).
                offset_saved2 = if offset_saved1 != 0 && rep_offset1 != 0 {
                    offset_saved1
                } else {
                    offset_saved2
                };

                // save reps for next block
                rep[0] = if rep_offset1 != 0 {
                    rep_offset1
                } else {
                    offset_saved1
                };
                rep[1] = if rep_offset2 != 0 {
                    rep_offset2
                } else {
                    offset_saved2
                };

                // Return the last literals size
                return iend - anchor;
            }
            Found::Offset {
                ip0,
                match_idx,
                current0,
            } => {
                // _offset: Compute the offset code.
                let mut ip = ip0;
                let mut match0 = window.pos(match_idx);
                rep_offset2 = rep_offset1;
                rep_offset1 = (ip - match0) as u32;
                let off_base = offset_to_offbase(rep_offset1);
                let mut m_length = 4;

                // Count the backwards match length.
                while ((ip > anchor) & (match0 > prefix_start)) && (src[ip - 1] == src[match0 - 1])
                {
                    ip -= 1;
                    match0 -= 1;
                    m_length += 1;
                }
                (ip, match0, m_length, off_base, current0)
            }
            Found::Match {
                ip0,
                match0,
                m_length,
                current0,
            } => (ip0, match0, m_length, REPCODE1_TO_OFFBASE, current0),
        };

        // _match: Count the forward length.
        let m_length = m_length + zstd_count(src, ip + m_length, match0 + m_length, iend);

        seq_store.store_seq(&src[anchor..ip], off_base, m_length);

        ip0 = ip + m_length;
        anchor = ip0;

        // Fill table and check for immediate repcode.
        if ip0 as isize <= ilimit {
            // Fill Table
            let p = window.pos(current0 + 2);
            ms.hash_table[hash_ptr(src, p, hlog, mls)] = current0 + 2; // here because current+2 could be > iend-8
            ms.hash_table[hash_ptr(src, ip0 - 2, hlog, mls)] = window.idx(ip0 - 2);

            if rep_offset2 > 0 {
                // rep_offset2==0 means rep_offset2 is invalidated
                while (ip0 as isize <= ilimit)
                    && (read32(src, ip0) == read32(src, ip0 - rep_offset2 as usize))
                {
                    // store sequence
                    let r_length =
                        zstd_count(src, ip0 + 4, ip0 + 4 - rep_offset2 as usize, iend) + 4;
                    std::mem::swap(&mut rep_offset1, &mut rep_offset2); // swap rep_offset2 <=> rep_offset1
                    ms.hash_table[hash_ptr(src, ip0, hlog, mls)] = window.idx(ip0);
                    ip0 += r_length;
                    seq_store.store_seq(&[], REPCODE1_TO_OFFBASE, r_length);
                    anchor = ip0;
                }
            }
        }
    }
}

/// Port of `ZSTD_compressBlock_fast` (external/zstd/lib/compress/zstd_fast.c:443-478
/// @ zstd v1.5.7), without a dictionary: the minimum match 4 to 7 (3 as 4).
pub(crate) fn compress_block_fast(
    ms: &mut MatchState,
    seq_store: &mut SeqStore,
    rep: &mut [u32; 3],
    src: &[u8],
    istart: usize,
    iend: usize,
) -> usize {
    let mls = match ms.cparams.min_match {
        m @ 5..=7 => m,
        _ => 4, // includes case 3
    };
    compress_block_fast_no_dict_generic(ms, seq_store, rep, src, istart, iend, mls)
}
