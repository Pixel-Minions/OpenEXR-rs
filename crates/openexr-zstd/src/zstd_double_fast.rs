// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_double_fast.c` (external/zstd/lib/compress/zstd_double_fast.c:1-778
//! @ zstd v1.5.7): the `dfast` strategy's block compressor, without a dictionary
//! (`ZSTD_compressCCtx` uses no other): a table of 8-byte hashes (`hashLong`, the match
//! state's hash table) and one of `minMatch`-byte hashes (`hashSmall`, its chain table). The
//! dictionary and external-dictionary variants and the table fill for dictionaries are n/a.

use crate::zstd_compress_internal::{
    HASH_READ_SIZE, K_SEARCH_STRENGTH, MatchState, REPCODE1_TO_OFFBASE, SeqStore,
    get_lowest_prefix_index, hash_ptr, offset_to_offbase, read32, read64, zstd_count,
};

/// Where the inner search loop left: upstream's labels `_cleanup`, `_search_next_long`,
/// `_match_found` (`Matched`) and `_match_stored` (a repcode match, already stored).
enum Found {
    Cleanup,
    SearchNextLong {
        ip: usize,
        ip1: usize,
        idxs0: u32,
        idxl1: u32,
        hl1: usize,
        step: usize,
    },
    Matched {
        ip: usize,
        offset: u32,
        m_length: usize,
        ip1: usize,
        hl1: usize,
        step: usize,
    },
    MatchStored {
        ip: usize,
        m_length: usize,
    },
}

/// Port of `ZSTD_compressBlock_doubleFast_noDict_generic`
/// (external/zstd/lib/compress/zstd_double_fast.c:103-324 @ zstd v1.5.7): the block
/// `src[istart..iend]` parsed into `seq_store`; returns the last literals' size. Upstream
/// reads a dummy array instead of a candidate below the prefix (`ZSTD_selectAddr`); such a
/// candidate never matches, here as there.
#[allow(clippy::too_many_arguments)]
fn compress_block_double_fast_no_dict_generic(
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
    let h_bits_l = cparams.hash_log;
    let h_bits_s = cparams.chain_log;
    let mut anchor = istart;
    let end_index = window.idx(iend);
    // presumes that, if there is a dictionary, it must be using Attach mode
    let prefix_lowest_index = get_lowest_prefix_index(ms, end_index, cparams.window_log);
    let prefix_lowest = window.pos(prefix_lowest_index);
    let ilimit = iend as isize - HASH_READ_SIZE as isize;
    let mut offset_1 = rep[0];
    let mut offset_2 = rep[1];
    let mut offset_saved1 = 0u32;
    let mut offset_saved2 = 0u32;

    // how many positions to search before increasing step size
    let k_step_incr = 1usize << K_SEARCH_STRENGTH;

    let mut ip = istart;
    // The index of the last position searched, for the complementary insertion.
    let mut curr: u32 = 0;

    // init
    ip += usize::from(ip == prefix_lowest);
    {
        let current = window.idx(ip);
        let window_low = get_lowest_prefix_index(ms, current, cparams.window_log);
        let max_rep = current - window_low;
        if offset_2 > max_rep {
            offset_saved2 = offset_2;
            offset_2 = 0;
        }
        if offset_1 > max_rep {
            offset_saved1 = offset_1;
            offset_1 = 0;
        }
    }

    // Outer Loop: one iteration per match found and stored
    loop {
        let mut step = 1usize;
        let mut next_step = ip + k_step_incr;
        let mut ip1 = ip + step;

        let found = if ip1 as isize > ilimit {
            Found::Cleanup
        } else {
            let mut hl0 = hash_ptr(src, ip, h_bits_l, 8);
            let mut idxl0 = ms.hash_table[hl0];

            // Inner Loop: one iteration per search / position
            loop {
                let hs0 = hash_ptr(src, ip, h_bits_s, mls);
                let idxs0 = ms.chain_table[hs0];
                curr = window.idx(ip);

                // update hash tables
                ms.hash_table[hl0] = curr;
                ms.chain_table[hs0] = curr;

                // check noDict repcode
                if (offset_1 > 0) & (read32(src, ip + 1 - offset_1 as usize) == read32(src, ip + 1))
                {
                    let m_length =
                        zstd_count(src, ip + 1 + 4, ip + 1 + 4 - offset_1 as usize, iend) + 4;
                    ip += 1;
                    seq_store.store_seq(&src[anchor..ip], REPCODE1_TO_OFFBASE, m_length);
                    break Found::MatchStored { ip, m_length };
                }

                let hl1 = hash_ptr(src, ip1, h_bits_l, 8);

                // check prefix long match
                if idxl0 >= prefix_lowest_index {
                    let matchl0 = window.pos(idxl0);
                    if read64(src, matchl0) == read64(src, ip) {
                        let mut m_length = zstd_count(src, ip + 8, matchl0 + 8, iend) + 8;
                        let offset = (ip - matchl0) as u32;
                        let (mut ip_, mut m) = (ip, matchl0);
                        // catch up
                        while ((ip_ > anchor) & (m > prefix_lowest)) && (src[ip_ - 1] == src[m - 1])
                        {
                            ip_ -= 1;
                            m -= 1;
                            m_length += 1;
                        }
                        break Found::Matched {
                            ip: ip_,
                            offset,
                            m_length,
                            ip1,
                            hl1,
                            step,
                        };
                    }
                }

                let idxl1 = ms.hash_table[hl1];

                // check prefix short match
                if idxs0 >= prefix_lowest_index && read32(src, window.pos(idxs0)) == read32(src, ip)
                {
                    break Found::SearchNextLong {
                        ip,
                        ip1,
                        idxs0,
                        idxl1,
                        hl1,
                        step,
                    };
                }

                if ip1 >= next_step {
                    step += 1;
                    next_step += k_step_incr;
                }
                ip = ip1;
                ip1 += step;

                hl0 = hl1;
                idxl0 = idxl1;
                if ip1 as isize > ilimit {
                    break Found::Cleanup;
                }
            }
        };

        let (match_ip, m_length) = match found {
            Found::Cleanup => {
                // If offset_1 started invalid (offsetSaved1 != 0) and became valid
                // (offset_1 != 0), rotate saved offsets.
                offset_saved2 = if offset_saved1 != 0 && offset_1 != 0 {
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
                return iend - anchor;
            }
            Found::MatchStored { ip, m_length } => (ip, m_length),
            other => {
                let (ip, offset, m_length, ip1, hl1, step) = match other {
                    Found::SearchNextLong {
                        ip,
                        ip1,
                        idxs0,
                        idxl1,
                        hl1,
                        step,
                    } => {
                        // short match found: let's check for a longer one
                        let mut matchs0 = window.pos(idxs0);
                        let mut m_length = zstd_count(src, ip + 4, matchs0 + 4, iend) + 4;
                        let mut offset = (ip - matchs0) as u32;
                        let mut ip_ = ip;

                        // check long match at +1 position
                        if idxl1 > prefix_lowest_index {
                            let matchl1 = window.pos(idxl1);
                            if read64(src, matchl1) == read64(src, ip1) {
                                let l1len = zstd_count(src, ip1 + 8, matchl1 + 8, iend) + 8;
                                if l1len > m_length {
                                    // use the long match instead
                                    ip_ = ip1;
                                    m_length = l1len;
                                    offset = (ip_ - matchl1) as u32;
                                    matchs0 = matchl1;
                                }
                            }
                        }

                        // complete backward
                        while ((ip_ > anchor) & (matchs0 > prefix_lowest))
                            && (src[ip_ - 1] == src[matchs0 - 1])
                        {
                            ip_ -= 1;
                            matchs0 -= 1;
                            m_length += 1;
                        }
                        (ip_, offset, m_length, ip1, hl1, step)
                    }
                    Found::Matched {
                        ip,
                        offset,
                        m_length,
                        ip1,
                        hl1,
                        step,
                    } => (ip, offset, m_length, ip1, hl1, step),
                    _ => unreachable!("handled above"),
                };

                // _match_found: requires ip, offset, mLength
                offset_2 = offset_1;
                offset_1 = offset;

                if step < 4 {
                    // The minmatch even if we take a short match is 4 bytes, so as long as
                    // step, the distance between ip and ip1 (initially) is less than 4, we
                    // know ip1 < new ip.
                    ms.hash_table[hl1] = window.idx(ip1);
                }

                seq_store.store_seq(&src[anchor..ip], offset_to_offbase(offset), m_length);
                (ip, m_length)
            }
        };

        // _match_stored: match found
        ip = match_ip + m_length;
        anchor = ip;

        if ip as isize <= ilimit {
            // Complementary insertion: done after iLimit test, as candidates could be > iend-8
            let index_to_insert = curr + 2;
            let p = window.pos(index_to_insert);
            ms.hash_table[hash_ptr(src, p, h_bits_l, 8)] = index_to_insert;
            ms.hash_table[hash_ptr(src, ip - 2, h_bits_l, 8)] = window.idx(ip - 2);
            ms.chain_table[hash_ptr(src, p, h_bits_s, mls)] = index_to_insert;
            ms.chain_table[hash_ptr(src, ip - 1, h_bits_s, mls)] = window.idx(ip - 1);

            // check immediate repcode
            while (ip as isize <= ilimit)
                && ((offset_2 > 0) & (read32(src, ip) == read32(src, ip - offset_2 as usize)))
            {
                // store sequence
                let r_length = zstd_count(src, ip + 4, ip + 4 - offset_2 as usize, iend) + 4;
                std::mem::swap(&mut offset_2, &mut offset_1); // swap offset_2 <=> offset_1
                ms.chain_table[hash_ptr(src, ip, h_bits_s, mls)] = window.idx(ip);
                ms.hash_table[hash_ptr(src, ip, h_bits_l, 8)] = window.idx(ip);
                seq_store.store_seq(&[], REPCODE1_TO_OFFBASE, r_length);
                ip += r_length;
                anchor = ip;
            }
        }
    }
}

/// Port of `ZSTD_compressBlock_doubleFast` (external/zstd/lib/compress/zstd_double_fast.c:568-586
/// @ zstd v1.5.7), without a dictionary: the minimum match 4 to 7 (3 as 4).
pub(crate) fn compress_block_double_fast(
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
    compress_block_double_fast_no_dict_generic(ms, seq_store, rep, src, istart, iend, mls)
}
