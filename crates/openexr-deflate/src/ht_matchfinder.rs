// SPDX-License-Identifier: MIT
// Copyright 2022 Eric Biggers

//! Port of `ht_matchfinder.h` (external/deflate/lib/ht_matchfinder.h:1-234 @ libdeflate
//! v1.26): Lempel-Ziv matchfinding with a hash table where each bucket is a fixed-size array
//! of the most recent positions, for the fastest compressor (level 1), for x86-64
//! (`UNALIGNED_ACCESS_IS_FAST`, buckets of 2).

use crate::matchfinder_common::{
    MATCHFINDER_WINDOW_SIZE, MfPos, load_u32, lz_extend, lz_hash, matchfinder_init,
    matchfinder_rebase,
};

/// Port of `HT_MATCHFINDER_HASH_ORDER` (external/deflate/lib/ht_matchfinder.h:50 @ libdeflate
/// v1.26).
const HT_MATCHFINDER_HASH_ORDER: u32 = 15;
/// Port of `HT_MATCHFINDER_BUCKET_SIZE` (external/deflate/lib/ht_matchfinder.h:51 @ libdeflate
/// v1.26).
const HT_MATCHFINDER_BUCKET_SIZE: usize = 2;
/// Port of `HT_MATCHFINDER_REQUIRED_NBYTES` (external/deflate/lib/ht_matchfinder.h:55
/// @ libdeflate v1.26): the bytes `longest_match` reads from the current position.
pub(crate) const HT_MATCHFINDER_REQUIRED_NBYTES: u32 = 5;

const WINDOW: usize = MATCHFINDER_WINDOW_SIZE as usize;

/// Port of `struct ht_matchfinder` (external/deflate/lib/ht_matchfinder.h:57-60 @ libdeflate
/// v1.26): `hash_tab[hash][i]`, at `hash * HT_MATCHFINDER_BUCKET_SIZE + i`.
pub(crate) struct HtMatchfinder {
    hash_tab: Vec<MfPos>,
}

impl HtMatchfinder {
    pub(crate) fn new() -> HtMatchfinder {
        HtMatchfinder {
            hash_tab: vec![0; (1 << HT_MATCHFINDER_HASH_ORDER) * HT_MATCHFINDER_BUCKET_SIZE],
        }
    }

    /// Port of `ht_matchfinder_init` (external/deflate/lib/ht_matchfinder.h:62-68 @ libdeflate
    /// v1.26).
    pub(crate) fn init(&mut self) {
        matchfinder_init(&mut self.hash_tab);
    }

    /// Port of `ht_matchfinder_slide_window` (external/deflate/lib/ht_matchfinder.h:70-74
    /// @ libdeflate v1.26).
    fn slide_window(&mut self) {
        matchfinder_rebase(&mut self.hash_tab);
    }

    /// Port of `ht_matchfinder_longest_match` (external/deflate/lib/ht_matchfinder.h:77-195
    /// @ libdeflate v1.26), the version for buckets of 2: the longest match of at least 4
    /// bytes for the string at `in_next` of `data` among the bucket's two positions (0 when
    /// there is none), stopping at `nice_len`; inserts the string. `next_hash` is its hash
    /// code, replaced by the next string's.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn longest_match(
        &mut self,
        data: &[u8],
        in_base_p: &mut usize,
        in_next: usize,
        max_len: u32,
        nice_len: u32,
        next_hash: &mut u32,
        offset_ret: &mut u32,
    ) -> u32 {
        let mut best_len = 0u32;
        let mut best_matchptr = in_next;
        let mut cur_pos = (in_next - *in_base_p) as u32;

        if cur_pos == MATCHFINDER_WINDOW_SIZE {
            self.slide_window();
            *in_base_p += WINDOW;
            cur_pos = 0;
        }
        let in_base = *in_base_p as isize;
        let cutoff = (cur_pos as i32 - MATCHFINDER_WINDOW_SIZE as i32) as MfPos;
        let at = |node: MfPos| (in_base + node as isize) as usize;

        let hash = *next_hash as usize * HT_MATCHFINDER_BUCKET_SIZE;
        *next_hash = lz_hash(load_u32(data, in_next + 1), HT_MATCHFINDER_HASH_ORDER);
        let seq = load_u32(data, in_next);

        // Hand-unrolled version for BUCKET_SIZE == 2. The logic here also differs slightly in
        // that it copies the first entry to the second even if nice_len is reached on the
        // first, as this can be slightly faster.
        'out: {
            let mut cur_node = self.hash_tab[hash];
            self.hash_tab[hash] = cur_pos as MfPos;
            if cur_node <= cutoff {
                break 'out;
            }
            let mut matchptr = at(cur_node);

            // to_insert = cur_node; cur_node = hash_tab[hash][1]; hash_tab[hash][1] = to_insert
            std::mem::swap(&mut cur_node, &mut self.hash_tab[hash + 1]);

            if load_u32(data, matchptr) == seq {
                best_len = lz_extend(data, in_next, matchptr, 4, max_len);
                best_matchptr = matchptr;
                if cur_node <= cutoff || best_len >= nice_len {
                    break 'out;
                }
                matchptr = at(cur_node);
                let b = best_len as usize;
                if load_u32(data, matchptr) == seq
                    && load_u32(data, matchptr + b - 3) == load_u32(data, in_next + b - 3)
                {
                    let len = lz_extend(data, in_next, matchptr, 4, max_len);
                    if len > best_len {
                        best_len = len;
                        best_matchptr = matchptr;
                    }
                }
            } else {
                if cur_node <= cutoff {
                    break 'out;
                }
                matchptr = at(cur_node);
                if load_u32(data, matchptr) == seq {
                    best_len = lz_extend(data, in_next, matchptr, 4, max_len);
                    best_matchptr = matchptr;
                }
            }
        }
        *offset_ret = (in_next - best_matchptr) as u32;
        best_len
    }

    /// Port of `ht_matchfinder_skip_bytes` (external/deflate/lib/ht_matchfinder.h:197-232
    /// @ libdeflate v1.26): advances past `count` strings from `in_next` of `data` (whose end is
    /// `in_end`), inserting each; nothing is done when fewer than `count + 5` bytes remain.
    /// The window slides first when the strings would cross it.
    pub(crate) fn skip_bytes(
        &mut self,
        data: &[u8],
        in_base_p: &mut usize,
        mut in_next: usize,
        in_end: usize,
        count: u32,
        next_hash: &mut u32,
    ) {
        let mut cur_pos = (in_next - *in_base_p) as i32;
        let mut remaining = count;

        if (count + HT_MATCHFINDER_REQUIRED_NBYTES) as usize > in_end - in_next {
            return;
        }

        if (cur_pos as u32).wrapping_add(count).wrapping_sub(1) >= MATCHFINDER_WINDOW_SIZE {
            self.slide_window();
            *in_base_p += WINDOW;
            cur_pos -= MATCHFINDER_WINDOW_SIZE as i32;
        }

        let mut hash = *next_hash as usize;
        loop {
            let h = hash * HT_MATCHFINDER_BUCKET_SIZE;
            for i in (1..HT_MATCHFINDER_BUCKET_SIZE).rev() {
                self.hash_tab[h + i] = self.hash_tab[h + i - 1];
            }
            self.hash_tab[h] = cur_pos as MfPos;

            in_next += 1;
            hash = lz_hash(load_u32(data, in_next), HT_MATCHFINDER_HASH_ORDER) as usize;
            cur_pos += 1;
            remaining = remaining.wrapping_sub(1);
            if remaining == 0 {
                break;
            }
        }

        *next_hash = hash as u32;
    }
}
