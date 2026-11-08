// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `bt_matchfinder.h` (external/deflate/lib/bt_matchfinder.h:1-342 @ libdeflate
//! v1.26): Lempel-Ziv matchfinding with a hash table of binary trees, for the near-optimal
//! compressor, for x86-64 (`UNALIGNED_ACCESS_IS_FAST`, little-endian).
//!
//! The struct's three tables are one array, as upstream's struct is one block of memory that
//! `matchfinder_rebase` slides at once. Upstream leaves `child_tab` uninitialized; here it
//! starts zeroed, and as upstream, no entry is read before it is written: the children of a
//! node are read only while the node is in the window, and both are written when it is
//! inserted. The prefetches (`prefetchw`) change nothing and are left out.

use crate::matchfinder_common::{
    MATCHFINDER_INITVAL, MATCHFINDER_WINDOW_SIZE, MfPos, load_u24, load_u32, lz_extend, lz_hash,
    matchfinder_init, matchfinder_rebase,
};

/// Port of `BT_MATCHFINDER_HASH3_ORDER` (external/deflate/lib/bt_matchfinder.h:70
/// @ libdeflate v1.26).
const BT_MATCHFINDER_HASH3_ORDER: u32 = 16;
/// Port of `BT_MATCHFINDER_HASH3_WAYS` (external/deflate/lib/bt_matchfinder.h:71
/// @ libdeflate v1.26).
const BT_MATCHFINDER_HASH3_WAYS: usize = 2;
/// Port of `BT_MATCHFINDER_HASH4_ORDER` (external/deflate/lib/bt_matchfinder.h:72
/// @ libdeflate v1.26).
const BT_MATCHFINDER_HASH4_ORDER: u32 = 16;

const WINDOW: usize = MATCHFINDER_WINDOW_SIZE as usize;
/// Where `hash4_tab` and `child_tab` start in the array; `BT_MATCHFINDER_TOTAL_HASH_SIZE`
/// (external/deflate/lib/bt_matchfinder.h:74-76 @ libdeflate v1.26) is [`CHILD_AT`] entries.
const HASH4_AT: usize = (1 << BT_MATCHFINDER_HASH3_ORDER) * BT_MATCHFINDER_HASH3_WAYS;
const CHILD_AT: usize = HASH4_AT + (1 << BT_MATCHFINDER_HASH4_ORDER);

/// Port of `BT_MATCHFINDER_REQUIRED_NBYTES` (external/deflate/lib/bt_matchfinder.h:136
/// @ libdeflate v1.26): the minimum permissible value of `max_len` for
/// [`BtMatchfinder::get_matches`] and [`BtMatchfinder::skip_byte`]. There must be sufficiently
/// many bytes remaining to load a 32-bit integer from the *next* position.
pub(crate) const BT_MATCHFINDER_REQUIRED_NBYTES: u32 = 5;

/// Port of `struct lz_match` (external/deflate/lib/bt_matchfinder.h:79-86 @ libdeflate v1.26):
/// a match found by the matchfinder, its length and its offset back from the current
/// position. (The near-optimal compressor's match cache also keeps a count of matches and a
/// literal in one.)
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct LzMatch {
    pub(crate) length: u16,
    pub(crate) offset: u16,
}

/// Port of `struct bt_matchfinder` (external/deflate/lib/bt_matchfinder.h:88-101 @ libdeflate
/// v1.26): `hash3_tab`, the hash table for finding length 3 matches (2 ways); `hash4_tab`, the
/// roots of the binary trees for finding length 4+ matches; and `child_tab`, the left and right
/// children of the node of each position of the window.
pub(crate) struct BtMatchfinder {
    tabs: Vec<MfPos>,
}

impl BtMatchfinder {
    pub(crate) fn new() -> BtMatchfinder {
        BtMatchfinder {
            tabs: vec![0; CHILD_AT + 2 * WINDOW],
        }
    }

    /// Port of `bt_matchfinder_init` (external/deflate/lib/bt_matchfinder.h:104-111
    /// @ libdeflate v1.26): prepares the matchfinder for a new input buffer.
    pub(crate) fn init(&mut self) {
        matchfinder_init(&mut self.tabs[..CHILD_AT]);
    }

    /// Port of `bt_matchfinder_slide_window` (external/deflate/lib/bt_matchfinder.h:113-119
    /// @ libdeflate v1.26).
    pub(crate) fn slide_window(&mut self) {
        matchfinder_rebase(&mut self.tabs);
    }

    /// Port of `bt_left_child` (external/deflate/lib/bt_matchfinder.h:121-125 @ libdeflate
    /// v1.26): where the left child of `node` is.
    #[inline]
    fn left_child(node: i32) -> usize {
        CHILD_AT + 2 * (node & (WINDOW as i32 - 1)) as usize
    }

    /// Port of `bt_right_child` (external/deflate/lib/bt_matchfinder.h:127-131 @ libdeflate
    /// v1.26): where the right child of `node` is.
    #[inline]
    fn right_child(node: i32) -> usize {
        CHILD_AT + 2 * (node & (WINDOW as i32 - 1)) as usize + 1
    }

    /// Port of `bt_matchfinder_advance_one_byte` (external/deflate/lib/bt_matchfinder.h:140-262
    /// @ libdeflate v1.26): advances the matchfinder by one byte, recording the matches in
    /// `matches` from `lz_matchptr` on when `RECORD_MATCHES`; returns the next free slot.
    /// `in_base` is the matchfinder's base in `data`, `cur_pos` the position relative to it.
    #[allow(clippy::too_many_arguments)]
    #[inline]
    fn advance_one_byte<const RECORD_MATCHES: bool>(
        &mut self,
        data: &[u8],
        in_base: usize,
        cur_pos: i32,
        max_len: u32,
        nice_len: u32,
        max_search_depth: u32,
        next_hashes: &mut [u32; 2],
        matches: &mut [LzMatch],
        mut lz_matchptr: usize,
    ) -> usize {
        let in_next = in_base + cur_pos as usize;
        let mut depth_remaining = max_search_depth;
        let cutoff = cur_pos - MATCHFINDER_WINDOW_SIZE as i32;
        let mut best_len: u32 = 3;
        // the position in `data` of a node (which may be before the base)
        let at = |node: i32| (in_base as isize + node as isize) as usize;

        let next_hashseq = load_u32(data, in_next + 1);

        let hash3 = next_hashes[0] as usize;
        let hash4 = next_hashes[1] as usize;

        next_hashes[0] = lz_hash(next_hashseq & 0xFFFFFF, BT_MATCHFINDER_HASH3_ORDER);
        next_hashes[1] = lz_hash(next_hashseq, BT_MATCHFINDER_HASH4_ORDER);

        let h3 = hash3 * BT_MATCHFINDER_HASH3_WAYS;
        let mut cur_node = i32::from(self.tabs[h3]);
        self.tabs[h3] = cur_pos as MfPos;
        let cur_node_2 = i32::from(self.tabs[h3 + 1]);
        self.tabs[h3 + 1] = cur_node as MfPos;
        if RECORD_MATCHES && cur_node > cutoff {
            let seq3 = load_u24(data, in_next);
            if seq3 == load_u24(data, at(cur_node)) {
                matches[lz_matchptr] = LzMatch {
                    length: 3,
                    offset: (in_next - at(cur_node)) as u16,
                };
                lz_matchptr += 1;
            } else if cur_node_2 > cutoff && seq3 == load_u24(data, at(cur_node_2)) {
                matches[lz_matchptr] = LzMatch {
                    length: 3,
                    offset: (in_next - at(cur_node_2)) as u16,
                };
                lz_matchptr += 1;
            }
        }

        cur_node = i32::from(self.tabs[HASH4_AT + hash4]);
        self.tabs[HASH4_AT + hash4] = cur_pos as MfPos;

        let mut pending_lt_ptr = Self::left_child(cur_pos);
        let mut pending_gt_ptr = Self::right_child(cur_pos);

        if cur_node <= cutoff {
            self.tabs[pending_lt_ptr] = MATCHFINDER_INITVAL;
            self.tabs[pending_gt_ptr] = MATCHFINDER_INITVAL;
            return lz_matchptr;
        }

        let mut best_lt_len: u32 = 0;
        let mut best_gt_len: u32 = 0;
        let mut len: u32 = 0;

        loop {
            let matchptr = at(cur_node);

            if data[matchptr + len as usize] == data[in_next + len as usize] {
                len = lz_extend(data, in_next, matchptr, len + 1, max_len);
                if !RECORD_MATCHES || len > best_len {
                    if RECORD_MATCHES {
                        best_len = len;
                        matches[lz_matchptr] = LzMatch {
                            length: len as u16,
                            offset: (in_next - matchptr) as u16,
                        };
                        lz_matchptr += 1;
                    }
                    if len >= nice_len {
                        self.tabs[pending_lt_ptr] = self.tabs[Self::left_child(cur_node)];
                        self.tabs[pending_gt_ptr] = self.tabs[Self::right_child(cur_node)];
                        return lz_matchptr;
                    }
                }
            }

            if data[matchptr + len as usize] < data[in_next + len as usize] {
                self.tabs[pending_lt_ptr] = cur_node as MfPos;
                pending_lt_ptr = Self::right_child(cur_node);
                cur_node = i32::from(self.tabs[pending_lt_ptr]);
                best_lt_len = len;
                if best_gt_len < len {
                    len = best_gt_len;
                }
            } else {
                self.tabs[pending_gt_ptr] = cur_node as MfPos;
                pending_gt_ptr = Self::left_child(cur_node);
                cur_node = i32::from(self.tabs[pending_gt_ptr]);
                best_gt_len = len;
                if best_lt_len < len {
                    len = best_lt_len;
                }
            }

            if cur_node <= cutoff || {
                depth_remaining -= 1;
                depth_remaining == 0
            } {
                self.tabs[pending_lt_ptr] = MATCHFINDER_INITVAL;
                self.tabs[pending_gt_ptr] = MATCHFINDER_INITVAL;
                return lz_matchptr;
            }
        }
    }

    /// Port of `bt_matchfinder_get_matches` (external/deflate/lib/bt_matchfinder.h:296-315
    /// @ libdeflate v1.26): the matches with the current position, `cur_pos` from `in_base`
    /// (the base when the matchfinder was last initialized or slid), recorded in `matches`
    /// from `lz_matchptr` on, sorted by strictly increasing length and (non-strictly)
    /// increasing offset; at most `nice_len - 2` of them. `max_len` must be at least
    /// [`BT_MATCHFINDER_REQUIRED_NBYTES`], `nice_len` at most `max_len`, and
    /// `max_search_depth` at least 1. `next_hashes`, the precomputed hash codes of the
    /// sequence at the position, become those of the next one. Returns the next free slot
    /// (`lz_matchptr` when no matches were found).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn get_matches(
        &mut self,
        data: &[u8],
        in_base: usize,
        cur_pos: i32,
        max_len: u32,
        nice_len: u32,
        max_search_depth: u32,
        next_hashes: &mut [u32; 2],
        matches: &mut [LzMatch],
        lz_matchptr: usize,
    ) -> usize {
        self.advance_one_byte::<true>(
            data,
            in_base,
            cur_pos,
            max_len,
            nice_len,
            max_search_depth,
            next_hashes,
            matches,
            lz_matchptr,
        )
    }

    /// Port of `bt_matchfinder_skip_byte` (external/deflate/lib/bt_matchfinder.h:323-340
    /// @ libdeflate v1.26): advances the matchfinder without recording matches.
    pub(crate) fn skip_byte(
        &mut self,
        data: &[u8],
        in_base: usize,
        cur_pos: i32,
        nice_len: u32,
        max_search_depth: u32,
        next_hashes: &mut [u32; 2],
    ) {
        self.advance_one_byte::<false>(
            data,
            in_base,
            cur_pos,
            nice_len,
            nice_len,
            max_search_depth,
            next_hashes,
            &mut [],
            0,
        );
    }
}
