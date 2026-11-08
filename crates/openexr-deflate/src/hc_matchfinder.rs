// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `hc_matchfinder.h` (external/deflate/lib/hc_matchfinder.h:1-401 @ libdeflate
//! v1.26): Lempel-Ziv matchfinding with a hash table of linked lists, for the greedy and lazy
//! compressors, for x86-64 (`UNALIGNED_ACCESS_IS_FAST`).
//!
//! The struct's three tables are one array, as upstream's struct is one block of memory that
//! `matchfinder_rebase` slides at once. Upstream leaves `next_tab` uninitialized; here it
//! starts zeroed, and as upstream, no entry is read before it is written.

use crate::matchfinder_common::{
    MATCHFINDER_WINDOW_SIZE, MfPos, load_u24, load_u32, loaded_u32_to_u24, lz_extend, lz_hash,
    matchfinder_init, matchfinder_rebase,
};

/// Port of `HC_MATCHFINDER_HASH3_ORDER` (external/deflate/lib/hc_matchfinder.h:112
/// @ libdeflate v1.26).
const HC_MATCHFINDER_HASH3_ORDER: u32 = 15;
/// Port of `HC_MATCHFINDER_HASH4_ORDER` (external/deflate/lib/hc_matchfinder.h:113
/// @ libdeflate v1.26).
const HC_MATCHFINDER_HASH4_ORDER: u32 = 16;

const HASH3_SIZE: usize = 1 << HC_MATCHFINDER_HASH3_ORDER;
const HASH4_SIZE: usize = 1 << HC_MATCHFINDER_HASH4_ORDER;
const WINDOW: usize = MATCHFINDER_WINDOW_SIZE as usize;
/// Where `hash4_tab` and `next_tab` start in the array.
const HASH4_AT: usize = HASH3_SIZE;
const NEXT_AT: usize = HASH3_SIZE + HASH4_SIZE;

/// Port of `struct hc_matchfinder` (external/deflate/lib/hc_matchfinder.h:118-131 @ libdeflate
/// v1.26): `hash3_tab`, the hash table for finding length 3 matches; `hash4_tab`, the first
/// nodes of the linked lists of length 4 matches; and `next_tab`, the "next node" of each
/// position in the window.
pub(crate) struct HcMatchfinder {
    tabs: Box<[MfPos; TABS]>,
}

/// The three tables' length.
const TABS: usize = NEXT_AT + WINDOW;

// Indices into the tables, masked to their sizes: the masks change no index (a hash code is
// below its table's size, `lz_hash`; a position is below the window's), but let the
// compiler see each access is in bounds.
#[inline]
fn h3(hash3: usize) -> usize {
    hash3 & (HASH3_SIZE - 1)
}
#[inline]
fn h4(hash4: usize) -> usize {
    HASH4_AT + (hash4 & (HASH4_SIZE - 1))
}
#[inline]
fn nx(pos: usize) -> usize {
    NEXT_AT + (pos & (WINDOW - 1))
}

impl HcMatchfinder {
    pub(crate) fn new() -> HcMatchfinder {
        HcMatchfinder {
            tabs: vec![0; TABS].into_boxed_slice().try_into().unwrap(),
        }
    }

    /// Port of `hc_matchfinder_init` (external/deflate/lib/hc_matchfinder.h:134-141
    /// @ libdeflate v1.26): prepares the matchfinder for a new input buffer.
    pub(crate) fn init(&mut self) {
        matchfinder_init(&mut self.tabs[..NEXT_AT]);
    }

    /// Port of `hc_matchfinder_slide_window` (external/deflate/lib/hc_matchfinder.h:143-149
    /// @ libdeflate v1.26).
    fn slide_window(&mut self) {
        matchfinder_rebase(&mut self.tabs[..]);
    }

    /// `prefetchw` of the next string's two buckets (external/deflate/lib/hc_matchfinder.h:238-239,
    /// 395-396 @ libdeflate v1.26): an ordinary load of each, which nothing waits for, brings
    /// its cache line in ahead of use as upstream's prefetch does; nothing else changes.
    #[inline]
    fn prefetch(&self, next_hashes: &[u32; 2]) {
        std::hint::black_box(self.tabs[h3(next_hashes[0] as usize)]);
        std::hint::black_box(self.tabs[h4(next_hashes[1] as usize)]);
    }

    #[inline]
    fn next(&self, node: MfPos) -> MfPos {
        self.tabs[nx((i32::from(node) & (WINDOW as i32 - 1)) as usize)]
    }

    /// Port of `hc_matchfinder_longest_match` (external/deflate/lib/hc_matchfinder.h:182-338
    /// @ libdeflate v1.26): the longest match for the string at `in_next` of `data` that is
    /// longer than `best_len`, searching at most `max_search_depth` candidates, stopping at
    /// `nice_len`; inserts the string. `in_base_p` is the matchfinder's base (it slides a
    /// window when `in_next` reaches it), and `next_hashes` the string's hash codes, replaced
    /// by the next one's. Returns the length, with the offset in `offset_ret`; a length not
    /// above the given `best_len` means no match was found.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)] // forceinline upstream
    pub(crate) fn longest_match(
        &mut self,
        data: &[u8],
        in_base_p: &mut usize,
        in_next: usize,
        mut best_len: u32,
        max_len: u32,
        nice_len: u32,
        max_search_depth: u32,
        next_hashes: &mut [u32; 2],
        offset_ret: &mut u32,
    ) -> u32 {
        let mut depth_remaining = max_search_depth;
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

        'out: {
            if max_len < 5 {
                // can we read 4 bytes from 'in_next + 1'?
                break 'out;
            }

            // Get the precomputed hash codes.
            let hash3 = next_hashes[0] as usize;
            let hash4 = next_hashes[1] as usize;

            // From the hash buckets, get the first node of each linked list.
            let cur_node3 = self.tabs[h3(hash3)];
            let mut cur_node4 = self.tabs[h4(hash4)];

            // Update for length 3 matches. This replaces the singleton node in the 'hash3'
            // bucket with the node for the current sequence.
            self.tabs[h3(hash3)] = cur_pos as MfPos;

            // Update for length 4 matches. This prepends the node for the current sequence to
            // the linked list in the 'hash4' bucket.
            self.tabs[h4(hash4)] = cur_pos as MfPos;
            self.tabs[nx(cur_pos as usize)] = cur_node4;

            // Compute the next hash codes.
            let next_hashseq = load_u32(data, in_next + 1);
            next_hashes[0] = lz_hash(next_hashseq & 0xFFFFFF, HC_MATCHFINDER_HASH3_ORDER);
            next_hashes[1] = lz_hash(next_hashseq, HC_MATCHFINDER_HASH4_ORDER);
            self.prefetch(next_hashes);

            if best_len < 4 {
                // No match of length >= 4 found yet?

                // Check for a length 3 match if needed.
                if cur_node3 <= cutoff {
                    break 'out;
                }

                let seq4 = load_u32(data, in_next);

                if best_len < 3 {
                    let matchptr = at(cur_node3);
                    if load_u24(data, matchptr) == loaded_u32_to_u24(seq4) {
                        best_len = 3;
                        best_matchptr = matchptr;
                    }
                }

                // Check for a length 4 match.
                if cur_node4 <= cutoff {
                    break 'out;
                }

                let mut matchptr;
                loop {
                    // No length 4 match found yet. Check the first 4 bytes.
                    matchptr = at(cur_node4);

                    if load_u32(data, matchptr) == seq4 {
                        break;
                    }

                    // The first 4 bytes did not match. Keep trying.
                    cur_node4 = self.next(cur_node4);
                    depth_remaining = depth_remaining.wrapping_sub(1);
                    if cur_node4 <= cutoff || depth_remaining == 0 {
                        break 'out;
                    }
                }

                // Found a match of length >= 4. Extend it to its full length.
                best_matchptr = matchptr;
                best_len = lz_extend(data, in_next, best_matchptr, 4, max_len);
                if best_len >= nice_len {
                    break 'out;
                }
                cur_node4 = self.next(cur_node4);
                depth_remaining = depth_remaining.wrapping_sub(1);
                if cur_node4 <= cutoff || depth_remaining == 0 {
                    break 'out;
                }
            } else if cur_node4 <= cutoff || best_len >= nice_len {
                break 'out;
            }

            // Check for matches of length >= 5.
            loop {
                let mut matchptr;
                loop {
                    matchptr = at(cur_node4);

                    // Already found a length 4 match. Try for a longer match; start by
                    // checking either the last 4 bytes and the first 4 bytes, or the last
                    // byte. (The last byte, the one which would extend the match length by
                    // 1, is the most important.)
                    let b = best_len as usize;
                    if load_u32(data, matchptr + b - 3) == load_u32(data, in_next + b - 3)
                        && load_u32(data, matchptr) == load_u32(data, in_next)
                    {
                        break;
                    }

                    // Continue to the next node in the list.
                    cur_node4 = self.next(cur_node4);
                    depth_remaining = depth_remaining.wrapping_sub(1);
                    if cur_node4 <= cutoff || depth_remaining == 0 {
                        break 'out;
                    }
                }

                let len = lz_extend(data, in_next, matchptr, 4, max_len);
                if len > best_len {
                    // This is the new longest match.
                    best_len = len;
                    best_matchptr = matchptr;
                    if best_len >= nice_len {
                        break 'out;
                    }
                }

                // Continue to the next node in the list.
                cur_node4 = self.next(cur_node4);
                depth_remaining = depth_remaining.wrapping_sub(1);
                if cur_node4 <= cutoff || depth_remaining == 0 {
                    break 'out;
                }
            }
        }
        *offset_ret = (in_next - best_matchptr) as u32;
        best_len
    }

    /// Port of `hc_matchfinder_skip_bytes` (external/deflate/lib/hc_matchfinder.h:360-399
    /// @ libdeflate v1.26): advances the matchfinder past `count` strings from `in_next` of
    /// `data` (whose end is `in_end`), inserting each without searching; nothing is done when
    /// fewer than `count + 5` bytes remain.
    #[inline(always)] // forceinline upstream
    pub(crate) fn skip_bytes(
        &mut self,
        data: &[u8],
        in_base_p: &mut usize,
        mut in_next: usize,
        in_end: usize,
        count: u32,
        next_hashes: &mut [u32; 2],
    ) {
        let mut remaining = count;

        if (count as usize) + 5 > in_end - in_next {
            return;
        }

        let mut cur_pos = (in_next - *in_base_p) as u32;
        let mut hash3 = next_hashes[0] as usize;
        let mut hash4 = next_hashes[1] as usize;
        loop {
            if cur_pos == MATCHFINDER_WINDOW_SIZE {
                self.slide_window();
                *in_base_p += WINDOW;
                cur_pos = 0;
            }
            self.tabs[h3(hash3)] = cur_pos as MfPos;
            self.tabs[nx(cur_pos as usize)] = self.tabs[h4(hash4)];
            self.tabs[h4(hash4)] = cur_pos as MfPos;

            in_next += 1;
            let next_hashseq = load_u32(data, in_next);
            hash3 = lz_hash(next_hashseq & 0xFFFFFF, HC_MATCHFINDER_HASH3_ORDER) as usize;
            hash4 = lz_hash(next_hashseq, HC_MATCHFINDER_HASH4_ORDER) as usize;
            cur_pos += 1;
            remaining = remaining.wrapping_sub(1);
            if remaining == 0 {
                break;
            }
        }

        next_hashes[0] = hash3 as u32;
        next_hashes[1] = hash4 as u32;
        self.prefetch(next_hashes);
    }
}
