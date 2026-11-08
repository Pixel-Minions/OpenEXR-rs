// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `deflate_compress.c` (external/deflate/lib/deflate_compress.c:1-4135 @ libdeflate
//! v1.26): the DEFLATE compressor, for x86-64 (a 64-bit bit buffer, unaligned word access,
//! no bit-reversal instruction).
//!
//! The Huffman code construction, the block flushing (uncompressed, static and dynamic
//! blocks), the block splitting heuristic, and the compressors of levels 0 (uncompressed), 1
//! (fastest, on `ht_matchfinder`), 2 to 4 (greedy), 5 to 9 (lazy and lazy2, on
//! `hc_matchfinder`) and 10 to 12 (near-optimal, on `bt_matchfinder`): every level.
//!
//! Upstream overlays the precode and the full length codewords in a union (`c->o`), and the
//! parsers' data in another (`c->p`); they are separate fields here. Upstream reads none of
//! them through another, and the stale entries the overlay leaves (codewords of unused
//! symbols) are never output, so nothing changes. The input is one slice and every pointer
//! upstream keeps into it, or into the output, is an index. Custom allocators are n/a
//! (PLAN.md O8).

use crate::bt_matchfinder::{BT_MATCHFINDER_REQUIRED_NBYTES, BtMatchfinder, LzMatch};
use crate::deflate_constants::*;
use crate::hc_matchfinder::HcMatchfinder;
use crate::ht_matchfinder::{HT_MATCHFINDER_REQUIRED_NBYTES, HtMatchfinder};
use crate::matchfinder_common::MATCHFINDER_WINDOW_SIZE;

/// `MATCHFINDER_WINDOW_SIZE`, as an index.
const WINDOW_SIZE: usize = MATCHFINDER_WINDOW_SIZE as usize;

/// Port of `MIN_BLOCK_LENGTH` (external/deflate/lib/deflate_compress.c:66 @ libdeflate v1.26):
/// the minimum block length, in uncompressed bytes, except for the final block if needed.
const MIN_BLOCK_LENGTH: usize = 5000;

/// Port of `SOFT_MAX_BLOCK_LENGTH` (external/deflate/lib/deflate_compress.c:81 @ libdeflate
/// v1.26): the soft maximum block length, in uncompressed bytes, for the greedy, lazy, lazy2
/// and near-optimal compressors.
const SOFT_MAX_BLOCK_LENGTH: usize = 300000;

/// Port of `SEQ_STORE_LENGTH` (external/deflate/lib/deflate_compress.c:93 @ libdeflate v1.26):
/// the length of the sequence store of the greedy and lazy compressors.
const SEQ_STORE_LENGTH: usize = 50000;

/// Port of `FAST_SOFT_MAX_BLOCK_LENGTH` (external/deflate/lib/deflate_compress.c:102 @ libdeflate
/// v1.26): the soft maximum block length of the fastest compressor.
const FAST_SOFT_MAX_BLOCK_LENGTH: usize = 65535;

/// Port of `FAST_SEQ_STORE_LENGTH` (external/deflate/lib/deflate_compress.c:108 @ libdeflate
/// v1.26): the length of the fastest compressor's sequence store.
const FAST_SEQ_STORE_LENGTH: usize = 8192;

/// Port of `MAX_LITLEN_CODEWORD_LEN` (external/deflate/lib/deflate_compress.c:117 @ libdeflate
/// v1.26): the maximum codeword length used for the litlen code, below the format's 15 so
/// that a block can be output with fewer flushes.
const MAX_LITLEN_CODEWORD_LEN: u32 = 14;
/// Port of `MAX_OFFSET_CODEWORD_LEN` (external/deflate/lib/deflate_compress.c:118 @ libdeflate
/// v1.26).
const MAX_OFFSET_CODEWORD_LEN: u32 = DEFLATE_MAX_OFFSET_CODEWORD_LEN;
/// Port of `MAX_PRE_CODEWORD_LEN` (external/deflate/lib/deflate_compress.c:119 @ libdeflate
/// v1.26).
const MAX_PRE_CODEWORD_LEN: u32 = DEFLATE_MAX_PRE_CODEWORD_LEN;

/// Port of `deflate_length_slot_base` (external/deflate/lib/deflate_compress.c:237-242
/// @ libdeflate v1.26): length slot => length slot base value.
const DEFLATE_LENGTH_SLOT_BASE: [u32; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];

/// Port of `deflate_extra_length_bits` (external/deflate/lib/deflate_compress.c:245-250
/// @ libdeflate v1.26): length slot => number of extra length bits.
const DEFLATE_EXTRA_LENGTH_BITS: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];

/// Port of `deflate_offset_slot_base` (external/deflate/lib/deflate_compress.c:253-258
/// @ libdeflate v1.26): offset slot => offset slot base value.
const DEFLATE_OFFSET_SLOT_BASE: [u32; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];

/// Port of `deflate_extra_offset_bits` (external/deflate/lib/deflate_compress.c:261-266
/// @ libdeflate v1.26): offset slot => number of extra offset bits.
const DEFLATE_EXTRA_OFFSET_BITS: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

/// Port of `deflate_length_slot` (external/deflate/lib/deflate_compress.c:269-285 @ libdeflate
/// v1.26): length => length slot.
const DEFLATE_LENGTH_SLOT: [u8; DEFLATE_MAX_MATCH_LEN + 1] = [
    0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13,
    14, 14, 14, 14, 15, 15, 15, 15, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 17, 17, 17,
    18, 18, 18, 18, 18, 18, 18, 18, 19, 19, 19, 19, 19, 19, 19, 19, 20, 20, 20, 20, 20, 20, 20, 20,
    20, 20, 20, 20, 20, 20, 20, 20, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21, 21,
    22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 22, 23, 23, 23, 23, 23, 23, 23, 23,
    23, 23, 23, 23, 23, 23, 23, 23, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24,
    24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 25,
    25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25, 25,
    26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26, 26,
    26, 26, 26, 26, 26, 26, 26, 26, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 28,
];

/// Port of `deflate_offset_slot` (external/deflate/lib/deflate_compress.c:291-308 @ libdeflate
/// v1.26): 'offset - 1 => offset_slot' for offset <= 256.
const DEFLATE_OFFSET_SLOT: [u8; 256] = [
    0, 1, 2, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 8, 8, 8, 8, 9, 9, 9, 9, 9, 9, 9, 9,
    10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 11, 11, 11, 11, 11, 11, 11, 11,
    11, 11, 11, 11, 11, 11, 11, 11, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12,
    12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 12, 13, 13, 13, 13, 13, 13, 13, 13,
    13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13, 13,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14,
    14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    15, 15, 15, 15, 15, 15, 15, 15,
];

/// Port of `deflate_precode_lens_permutation` (external/deflate/lib/deflate_compress.c:311-313
/// @ libdeflate v1.26): the order in which precode codeword lengths are stored.
const DEFLATE_PRECODE_LENS_PERMUTATION: [u8; DEFLATE_NUM_PRECODE_SYMS] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Port of `deflate_extra_precode_bits` (external/deflate/lib/deflate_compress.c:316-318
/// @ libdeflate v1.26): precode symbol => number of extra bits.
const DEFLATE_EXTRA_PRECODE_BITS: [u8; DEFLATE_NUM_PRECODE_SYMS] =
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 3, 7];

/// Where the offset code's lengths start in [`Codes::lens`]: upstream's `struct deflate_lens`
/// holds the litlen lengths and then the offset lengths, and its header code relies on it.
const LENS_OFFSET: usize = DEFLATE_NUM_LITLEN_SYMS;

/// Port of `struct deflate_codes` (external/deflate/lib/deflate_compress.c:320-339
/// @ libdeflate v1.26): the codewords and lengths (in bits) of the DEFLATE Huffman codes; a
/// zero length means the symbol had zero frequency.
#[derive(Clone)]
struct Codes {
    codewords_litlen: [u32; DEFLATE_NUM_LITLEN_SYMS],
    codewords_offset: [u32; DEFLATE_NUM_OFFSET_SYMS],
    /// `lens.litlen`, then `lens.offset` (at [`LENS_OFFSET`]).
    lens: [u8; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS],
}

impl Codes {
    fn new() -> Codes {
        Codes {
            codewords_litlen: [0; DEFLATE_NUM_LITLEN_SYMS],
            codewords_offset: [0; DEFLATE_NUM_OFFSET_SYMS],
            lens: [0; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS],
        }
    }
}

/// Port of `struct deflate_freqs` (external/deflate/lib/deflate_compress.c:342-345
/// @ libdeflate v1.26): symbol frequency counters for the DEFLATE Huffman codes.
#[derive(Clone)]
struct Freqs {
    litlen: [u32; DEFLATE_NUM_LITLEN_SYMS],
    offset: [u32; DEFLATE_NUM_OFFSET_SYMS],
}

/// Port of `SEQ_LENGTH_SHIFT` (external/deflate/lib/deflate_compress.c:365 @ libdeflate v1.26).
const SEQ_LENGTH_SHIFT: u32 = 23;
/// Port of `SEQ_LITRUNLEN_MASK` (external/deflate/lib/deflate_compress.c:366 @ libdeflate v1.26).
const SEQ_LITRUNLEN_MASK: u32 = (1 << SEQ_LENGTH_SHIFT) - 1;

/// Port of `struct deflate_sequence` (external/deflate/lib/deflate_compress.c:353-380
/// @ libdeflate v1.26): a run of literals followed by a match or end-of-block. Bits 0..22 of
/// `litrunlen_and_length` are the number of literals, bits 23..31 the length of the match
/// which follows them, or 0 for the last run of the block.
#[derive(Clone, Copy, Default)]
struct Sequence {
    litrunlen_and_length: u32,
    offset: u16,
    offset_slot: u16,
}

// Block split statistics (external/deflate/lib/deflate_compress.c:439-443 @ libdeflate v1.26).
const NUM_LITERAL_OBSERVATION_TYPES: usize = 8;
const NUM_MATCH_OBSERVATION_TYPES: usize = 2;
const NUM_OBSERVATION_TYPES: usize = NUM_LITERAL_OBSERVATION_TYPES + NUM_MATCH_OBSERVATION_TYPES;
const NUM_OBSERVATIONS_PER_BLOCK_CHECK: u32 = 512;

/// Port of `struct block_split_stats` (external/deflate/lib/deflate_compress.c:444-449
/// @ libdeflate v1.26).
#[derive(Clone, Default)]
struct BlockSplitStats {
    new_observations: [u32; NUM_OBSERVATION_TYPES],
    observations: [u32; NUM_OBSERVATION_TYPES],
    num_new_observations: u32,
    num_observations: u32,
}

/// The precode half of upstream's `c->o` union (external/deflate/lib/deflate_compress.c:494-505
/// @ libdeflate v1.26): information about the precode.
struct Precode {
    freqs: [u32; DEFLATE_NUM_PRECODE_SYMS],
    codewords: [u32; DEFLATE_NUM_PRECODE_SYMS],
    lens: [u8; DEFLATE_NUM_PRECODE_SYMS],
    items: [u32; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS],
    num_litlen_syms: usize,
    num_offset_syms: usize,
    num_explicit_lens: usize,
    num_items: usize,
}

/// The other half of `c->o`: the "full" length codewords, the litlen codeword of each match
/// length with its extra bits.
struct LengthCodes {
    codewords: [u32; DEFLATE_MAX_MATCH_LEN + 1],
    lens: [u8; DEFLATE_MAX_MATCH_LEN + 1],
}

/// The compressor a level uses (upstream's `c->impl`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Strategy {
    /// Level 0: no `impl`, every input is passed through as uncompressed blocks.
    None,
    Greedy,
    Lazy,
    Lazy2,
    Fastest,
    NearOptimal,
}

/// Port of `struct libdeflate_compressor` (external/deflate/lib/deflate_compress.c:454-662
/// @ libdeflate v1.26): a DEFLATE compressor for one compression level.
pub struct Compressor {
    strategy: Strategy,
    /// The compression level with which this compressor was created.
    compression_level: u32,
    /// Anything of this size or less we won't bother trying to compress.
    max_passthrough_size: usize,
    /// The maximum search depth: consider at most this many potential matches at each
    /// position.
    max_search_depth: u32,
    /// The "nice" match length: if a match of this length is found, choose it immediately
    /// without further consideration.
    nice_match_length: u32,
    /// Frequency counters for the current block.
    freqs: Freqs,
    /// Block split statistics for the current block.
    split_stats: BlockSplitStats,
    /// Dynamic Huffman codes for the current block.
    codes: Codes,
    /// The static Huffman codes defined by the DEFLATE format.
    static_codes: Codes,
    precode: Box<Precode>,
    length: Box<LengthCodes>,
    /// The greedy and lazy parsers' hash chains matchfinder (`c->p.g.hc_mf`).
    hc_mf: Option<HcMatchfinder>,
    /// The fastest parser's hash table matchfinder (`c->p.f.ht_mf`).
    ht_mf: Option<HtMatchfinder>,
    /// The parsers' matches and literals chosen for the current block (`c->p.g.sequences`,
    /// `c->p.f.sequences`).
    sequences: Vec<Sequence>,
    /// The near-optimal parser's data (`c->p.n`).
    n: Option<Box<NearOptimal>>,
}

/// What a block is output from: upstream's `sequences`, or, when they are NULL, the
/// near-optimal parser's minimum-cost path (`c->p.n.optimum_nodes`).
enum Items<'a> {
    Sequences(&'a [Sequence]),
    Optimum(&'a NearOptimal),
}

impl std::fmt::Debug for Compressor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compressor")
            .field("compression_level", &self.compression_level)
            .finish_non_exhaustive()
    }
}

/// Port of `struct deflate_output_bitstream` (external/deflate/lib/deflate_compress.c:688-710
/// @ libdeflate v1.26), with the output buffer: bits not yet written (`bitbuf`, `bitcount` of
/// them), and where the next byte goes.
struct OutputBitstream<'o> {
    bitbuf: u64,
    bitcount: u32,
    out: &'o mut [u8],
    next: usize,
    overflow: bool,
}

/// `WORDBYTES` of x86-64.
const WORDBYTES: usize = 8;

impl OutputBitstream<'_> {
    /// `ADD_BITS` (external/deflate/lib/deflate_compress.c:717-722 @ libdeflate v1.26): adds
    /// `n` bits to the bit buffer.
    #[inline]
    fn add_bits(&mut self, bits: u32, n: u32) {
        self.bitbuf |= u64::from(bits) << self.bitcount;
        self.bitcount += n;
    }

    /// `FLUSH_BITS` (external/deflate/lib/deflate_compress.c:734-751 @ libdeflate v1.26):
    /// writes the whole bytes of the bit buffer, a word at a time when a word fits (writing
    /// zeros past the bytes it advances over, which later writes replace, as upstream does).
    #[inline]
    fn flush_bits(&mut self, out_fast_end: usize) {
        if self.next < out_fast_end {
            // Flush a whole word (branchlessly).
            self.out[self.next..self.next + WORDBYTES].copy_from_slice(&self.bitbuf.to_le_bytes());
            self.bitbuf >>= self.bitcount & !7;
            self.next += (self.bitcount >> 3) as usize;
            self.bitcount &= 7;
        } else {
            // Flush a byte at a time.
            while self.bitcount >= 8 {
                self.out[self.next] = self.bitbuf as u8;
                self.next += 1;
                self.bitcount -= 8;
                self.bitbuf >>= 8;
            }
        }
    }
}

/// `CAN_BUFFER(n)` (external/deflate/lib/deflate_compress.c:682 @ libdeflate v1.26), with
/// `BITBUF_NBITS` 63: can `n` bits always be added to the bit buffer after a flush?
const fn can_buffer(n: u32) -> bool {
    7 + n <= 63
}

/// Port of `heapify_subtree` (external/deflate/lib/deflate_compress.c:759-777 @ libdeflate
/// v1.26): restores the maxheap property of the subtree at `subtree_idx` of `a`, whose
/// children already have it; indices are 1-based (`a[i - 1]` is node `i`).
fn heapify_subtree(a: &mut [u32], length: usize, subtree_idx: usize) {
    let v = a[subtree_idx - 1];
    let mut parent_idx = subtree_idx;
    loop {
        let mut child_idx = parent_idx * 2;
        if child_idx > length {
            break;
        }
        if child_idx < length && a[child_idx] > a[child_idx - 1] {
            child_idx += 1;
        }
        if v >= a[child_idx - 1] {
            break;
        }
        a[parent_idx - 1] = a[child_idx - 1];
        parent_idx = child_idx;
    }
    a[parent_idx - 1] = v;
}

/// Port of `heapify_array` (external/deflate/lib/deflate_compress.c:783-790 @ libdeflate
/// v1.26).
fn heapify_array(a: &mut [u32], length: usize) {
    let mut subtree_idx = length / 2;
    while subtree_idx >= 1 {
        heapify_subtree(a, length, subtree_idx);
        subtree_idx -= 1;
    }
}

/// Port of `heap_sort` (external/deflate/lib/deflate_compress.c:798-813 @ libdeflate v1.26):
/// sorts `a`.
fn heap_sort(a: &mut [u32]) {
    let mut length = a.len();
    heapify_array(a, length);
    while length >= 2 {
        a.swap(length - 1, 0);
        length -= 1;
        heapify_subtree(a, length, 1);
    }
}

const NUM_SYMBOL_BITS: u32 = 10;
const SYMBOL_MASK: u32 = (1 << NUM_SYMBOL_BITS) - 1;
const FREQ_MASK: u32 = !SYMBOL_MASK;

/// Port of `sort_symbols` (external/deflate/lib/deflate_compress.c:846-906 @ libdeflate
/// v1.26): the symbols of nonzero frequency, sorted by frequency and then by symbol, into
/// `symout` (each the symbol in the low [`NUM_SYMBOL_BITS`] bits and its frequency above);
/// the lengths of the others set to 0. Returns how many symbols have nonzero frequency.
fn sort_symbols(num_syms: usize, freqs: &[u32], lens: &mut [u8], symout: &mut [u32]) -> usize {
    let mut counters = [0u32; DEFLATE_MAX_NUM_SYMS];
    let num_counters = num_syms;

    // Count the frequencies.
    for &f in &freqs[..num_syms] {
        counters[(f as usize).min(num_counters - 1)] += 1;
    }

    // Make the counters cumulative, ignoring the zero-th, which counted symbols with zero
    // frequency. As a side effect, this calculates the number of symbols with nonzero
    // frequency.
    let mut num_used_syms = 0u32;
    for c in &mut counters[1..num_counters] {
        let count = *c;
        *c = num_used_syms;
        num_used_syms += count;
    }

    // Sort nonzero-frequency symbols using the counters. At the same time, set the codeword
    // lengths of zero-frequency symbols to 0.
    for sym in 0..num_syms {
        let freq = freqs[sym];
        if freq != 0 {
            let c = &mut counters[(freq as usize).min(num_counters - 1)];
            symout[*c as usize] = sym as u32 | (freq << NUM_SYMBOL_BITS);
            *c += 1;
        } else {
            lens[sym] = 0;
        }
    }

    // Sort the symbols counted in the last counter.
    let (lo, hi) = (
        counters[num_counters - 2] as usize,
        counters[num_counters - 1] as usize,
    );
    heap_sort(&mut symout[lo..hi]);

    num_used_syms as usize
}

/// Port of `build_tree` (external/deflate/lib/deflate_compress.c:939-995 @ libdeflate v1.26):
/// the non-leaf nodes of the Huffman tree of the `sym_count` (at least 2) sorted frequencies
/// in `a`, each the index of its parent above the low [`NUM_SYMBOL_BITS`] bits, which are kept;
/// the root is `a[sym_count - 2]`.
// (the C comparisons are kept as written)
#[allow(clippy::int_plus_one)]
fn build_tree(a: &mut [u32], sym_count: usize) {
    let last_idx = sym_count - 1;

    // Index of the next lowest frequency leaf that still needs a parent
    let mut i = 0usize;
    // Index of the next lowest frequency non-leaf that still needs a parent, or 'e' if there
    // is currently no such node
    let mut b = 0usize;
    // Index of the next spot for a non-leaf (will overwrite a leaf)
    let mut e = 0usize;

    loop {
        let new_freq;
        if i + 1 <= last_idx && (b == e || (a[i + 1] & FREQ_MASK) <= (a[b] & FREQ_MASK)) {
            // Two leaves
            new_freq = (a[i] & FREQ_MASK).wrapping_add(a[i + 1] & FREQ_MASK);
            i += 2;
        } else if b + 2 <= e && (i > last_idx || (a[b + 1] & FREQ_MASK) < (a[i] & FREQ_MASK)) {
            // Two non-leaves
            new_freq = (a[b] & FREQ_MASK).wrapping_add(a[b + 1] & FREQ_MASK);
            a[b] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b] & SYMBOL_MASK);
            a[b + 1] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b + 1] & SYMBOL_MASK);
            b += 2;
        } else {
            // One leaf and one non-leaf
            new_freq = (a[i] & FREQ_MASK).wrapping_add(a[b] & FREQ_MASK);
            a[b] = ((e as u32) << NUM_SYMBOL_BITS) | (a[b] & SYMBOL_MASK);
            i += 1;
            b += 1;
        }
        a[e] = new_freq | (a[e] & SYMBOL_MASK);
        // A binary tree with 'n' leaves has 'n - 1' non-leaves, so the tree is complete once
        // we've created 'n - 1' non-leaves.
        e += 1;
        if e >= last_idx {
            break;
        }
    }
}

/// Port of `compute_length_counts` (external/deflate/lib/deflate_compress.c:1022-1091
/// @ libdeflate v1.26): how many codewords get each length up to `max_codeword_len`, from the
/// tree [`build_tree`] made (which this destroys, keeping the low bits).
fn compute_length_counts(
    a: &mut [u32],
    root_idx: usize,
    len_counts: &mut [u32],
    max_codeword_len: usize,
) {
    for c in &mut len_counts[..=max_codeword_len] {
        *c = 0;
    }
    len_counts[1] = 2;

    // Set the root node's depth to 0.
    a[root_idx] &= SYMBOL_MASK;

    let mut node = root_idx as isize - 1;
    while node >= 0 {
        let n = node as usize;
        // Calculate the depth of this node.
        let parent = (a[n] >> NUM_SYMBOL_BITS) as usize;
        let parent_depth = a[parent] >> NUM_SYMBOL_BITS;
        let mut depth = (parent_depth + 1) as usize;

        // Set the depth of this node so that it is available when its children (if any) are
        // processed.
        a[n] = (a[n] & SYMBOL_MASK) | ((depth as u32) << NUM_SYMBOL_BITS);

        // If needed, decrease the length to meet the length-limited constraint. This is not
        // the optimal method for generating length-limited Huffman codes! But it should be
        // good enough.
        if depth >= max_codeword_len {
            depth = max_codeword_len;
            loop {
                depth -= 1;
                if len_counts[depth] != 0 {
                    break;
                }
            }
        }

        // Account for the fact that we have a non-leaf node at the current depth.
        len_counts[depth] -= 1;
        len_counts[depth + 1] += 2;
        node -= 1;
    }
}

/// Port of the table `reverse_codeword` (external/deflate/lib/deflate_compress.c:1108-1150
/// @ libdeflate v1.26): `codeword` (below 2^16) bit-reversed in `len` bits; 0 for length 0,
/// as the table version gives.
#[inline]
fn reverse_codeword(codeword: u32, len: u8) -> u32 {
    u32::from((codeword as u16).reverse_bits()) >> (16 - u32::from(len))
}

/// Port of `gen_codewords` (external/deflate/lib/deflate_compress.c:1177-1216 @ libdeflate
/// v1.26): each symbol's codeword length, from the sorted symbols in `a` and the length
/// counts, then the canonical codewords, bit-reversed, into `a`.
fn gen_codewords(
    a: &mut [u32],
    lens: &mut [u8],
    len_counts: &[u32],
    max_codeword_len: usize,
    num_syms: usize,
) {
    let mut next_codewords = [0u32; DEFLATE_MAX_CODEWORD_LEN + 1];

    let mut i = 0usize;
    for len in (1..=max_codeword_len).rev() {
        for _ in 0..len_counts[len] {
            lens[(a[i] & SYMBOL_MASK) as usize] = len as u8;
            i += 1;
        }
    }

    next_codewords[0] = 0;
    next_codewords[1] = 0;
    for len in 2..=max_codeword_len {
        next_codewords[len] = (next_codewords[len - 1] + len_counts[len - 1]) << 1;
    }

    for sym in 0..num_syms {
        // DEFLATE requires bit-reversed codewords.
        let l = lens[sym];
        let cw = next_codewords[l as usize];
        next_codewords[l as usize] = cw.wrapping_add(1);
        a[sym] = reverse_codeword(cw, l);
    }
}

/// Port of `deflate_make_huffman_code` (external/deflate/lib/deflate_compress.c:1318-1396
/// @ libdeflate v1.26): a length-limited canonical Huffman code for `num_syms` symbols of
/// frequencies `freqs`: the lengths into `lens` and the codewords into `codewords` (which
/// also serves as the work array, as upstream's does).
fn deflate_make_huffman_code(
    num_syms: usize,
    max_codeword_len: u32,
    freqs: &[u32],
    lens: &mut [u8],
    codewords: &mut [u32],
) {
    let num_used_syms = sort_symbols(num_syms, freqs, lens, codewords);

    // Handle special cases where only 0 or 1 symbols were used (had nonzero frequency).
    if num_used_syms < 2 {
        let sym = if num_used_syms != 0 {
            (codewords[0] & SYMBOL_MASK) as usize
        } else {
            0
        };
        let nonzero_idx = if sym != 0 { sym } else { 1 };

        codewords[0] = 0;
        lens[0] = 1;
        codewords[nonzero_idx] = 1;
        lens[nonzero_idx] = 1;
        return;
    }

    // Build a stripped-down version of the Huffman tree, sharing the array 'codewords' with
    // the symbol values. Then extract length counts from the tree and use them to generate
    // the final codewords.
    build_tree(codewords, num_used_syms);

    let mut len_counts = [0u32; DEFLATE_MAX_CODEWORD_LEN + 1];
    compute_length_counts(
        codewords,
        num_used_syms - 2,
        &mut len_counts,
        max_codeword_len as usize,
    );
    gen_codewords(
        codewords,
        lens,
        &len_counts,
        max_codeword_len as usize,
        num_syms,
    );
}

/// Port of `deflate_make_huffman_codes` (external/deflate/lib/deflate_compress.c:1414-1429
/// @ libdeflate v1.26): the litlen and offset codes of `freqs`.
fn deflate_make_huffman_codes(freqs: &Freqs, codes: &mut Codes) {
    let (litlen_lens, offset_lens) = codes.lens.split_at_mut(LENS_OFFSET);
    deflate_make_huffman_code(
        DEFLATE_NUM_LITLEN_SYMS,
        MAX_LITLEN_CODEWORD_LEN,
        &freqs.litlen,
        litlen_lens,
        &mut codes.codewords_litlen,
    );
    deflate_make_huffman_code(
        DEFLATE_NUM_OFFSET_SYMS,
        MAX_OFFSET_CODEWORD_LEN,
        &freqs.offset,
        offset_lens,
        &mut codes.codewords_offset,
    );
}

/// Port of `deflate_get_offset_slot` (external/deflate/lib/deflate_compress.c:1453-1480
/// @ libdeflate v1.26): the offset slot of a match offset, using the small map.
#[inline]
fn deflate_get_offset_slot(offset: u32) -> u32 {
    // 1 <= offset <= 32768 here. For 1 <= offset <= 256, deflate_offset_slot[offset - 1]
    // gives the slot.
    //
    // For 257 <= offset <= 32768, we take advantage of the fact that 257 is the beginning of
    // slot 16, and each slot [16..30) is exactly 1 << 7 == 128 times larger than each slot
    // [2..16) (since the number of extra bits increases by 1 every 2 slots). Thus, the slot
    // is:
    //
    //	deflate_offset_slot[2 + ((offset - 257) >> 7)] + (16 - 2)
    //   == deflate_offset_slot[((offset - 1) >> 7)] + 14
    //
    // Define 'n = (offset <= 256) ? 0 : 7'. Then any offset is handled by:
    //
    //      deflate_offset_slot[(offset - 1) >> n] + (n << 1)
    //
    // For better performance, replace 'n = (offset <= 256) ? 0 : 7' with the equivalent (for
    // offset <= 536871168) 'n = (256 - offset) >> 29'.
    let n = 256u32.wrapping_sub(offset) >> 29;
    u32::from(DEFLATE_OFFSET_SLOT[((offset - 1) >> n) as usize]) + (n << 1)
}

/// Port of `deflate_compute_precode_items` (external/deflate/lib/deflate_compress.c:1482-1557
/// @ libdeflate v1.26): the run-length coding of the codeword lengths `lens` with the precode
/// symbols (each item the symbol in bits 0..4 and its extra bits above), and the symbols'
/// frequencies. Returns the number of items.
fn deflate_compute_precode_items(
    lens: &[u8],
    precode_freqs: &mut [u32; DEFLATE_NUM_PRECODE_SYMS],
    precode_items: &mut [u32],
) -> usize {
    let num_lens = lens.len();
    *precode_freqs = [0; DEFLATE_NUM_PRECODE_SYMS];

    let mut itemptr = 0usize;
    let mut run_start = 0usize;
    loop {
        // Find the next run of codeword lengths.

        // len = the length being repeated
        let len = lens[run_start];

        // Extend the run.
        let mut run_end = run_start;
        loop {
            run_end += 1;
            if run_end == num_lens || len != lens[run_end] {
                break;
            }
        }

        if len == 0 {
            // Run of zeroes.

            // Symbol 18: RLE 11 to 138 zeroes at a time.
            while (run_end - run_start) >= 11 {
                let extra_bits = ((run_end - run_start) - 11).min(0x7F) as u32;
                precode_freqs[18] += 1;
                precode_items[itemptr] = 18 | (extra_bits << 5);
                itemptr += 1;
                run_start += 11 + extra_bits as usize;
            }

            // Symbol 17: RLE 3 to 10 zeroes at a time.
            if (run_end - run_start) >= 3 {
                let extra_bits = ((run_end - run_start) - 3).min(0x7) as u32;
                precode_freqs[17] += 1;
                precode_items[itemptr] = 17 | (extra_bits << 5);
                itemptr += 1;
                run_start += 3 + extra_bits as usize;
            }
        } else {
            // A run of nonzero lengths.

            // Symbol 16: RLE 3 to 6 of the previous length.
            if (run_end - run_start) >= 4 {
                precode_freqs[len as usize] += 1;
                precode_items[itemptr] = u32::from(len);
                itemptr += 1;
                run_start += 1;
                loop {
                    let extra_bits = ((run_end - run_start) - 3).min(0x3) as u32;
                    precode_freqs[16] += 1;
                    precode_items[itemptr] = 16 | (extra_bits << 5);
                    itemptr += 1;
                    run_start += 3 + extra_bits as usize;
                    if (run_end - run_start) < 3 {
                        break;
                    }
                }
            }
        }

        // Output any remaining lengths without RLE.
        while run_start != run_end {
            precode_freqs[len as usize] += 1;
            precode_items[itemptr] = u32::from(len);
            itemptr += 1;
            run_start += 1;
        }
        if run_start == num_lens {
            break;
        }
    }

    itemptr
}

/// Port of `adjust_max_and_nice_len` (external/deflate/lib/deflate_compress.c:2270-2277
/// @ libdeflate v1.26): decreases the maximum and nice match lengths near the end of the
/// input.
#[inline]
fn adjust_max_and_nice_len(max_len: &mut u32, nice_len: &mut u32, remaining: usize) {
    if remaining < DEFLATE_MAX_MATCH_LEN {
        *max_len = remaining as u32;
        *nice_len = (*nice_len).min(*max_len);
    }
}

/// Port of `choose_min_match_len` (external/deflate/lib/deflate_compress.c:2295-2327
/// @ libdeflate v1.26): the minimum match length for the greedy and lazy parsers, from the
/// number of different literals the data uses.
fn choose_min_match_len(num_used_literals: u32, max_search_depth: u32) -> u32 {
    // map from num_used_literals to min_len
    #[rustfmt::skip]
    const MIN_LENS: [u8; 80] = [
        9, 9, 9, 9, 9, 9, 8, 8, 7, 7, 6, 6, 6, 6, 6, 6,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5,
        5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4,
    ];
    // (the rest is implicitly 3)
    if num_used_literals as usize >= MIN_LENS.len() {
        return 3;
    }
    let mut min_len = u32::from(MIN_LENS[num_used_literals as usize]);
    // With a low max_search_depth, it may be too hard to find long matches.
    if max_search_depth < 16 {
        if max_search_depth < 5 {
            min_len = min_len.min(4);
        } else if max_search_depth < 10 {
            min_len = min_len.min(5);
        } else {
            min_len = min_len.min(7);
        }
    }
    min_len
}

/// Port of `calculate_min_match_len` (external/deflate/lib/deflate_compress.c:2329-2353
/// @ libdeflate v1.26): [`choose_min_match_len`] from the literals of the first 4 KiB of
/// `data`; 3 for less than 512 bytes.
fn calculate_min_match_len(data: &[u8], max_search_depth: u32) -> u32 {
    let mut used = [0u8; 256];
    let mut num_used_literals = 0u32;

    // For very short inputs, the static Huffman code has a good chance of being best, in
    // which case there is no reason to avoid short matches.
    if data.len() < 512 {
        return DEFLATE_MIN_MATCH_LEN as u32;
    }

    // For an initial approximation, scan the first 4 KiB of data. The caller may use
    // recalculate_min_match_len() to update min_len later.
    for &b in &data[..data.len().min(4096)] {
        used[b as usize] = 1;
    }
    for &u in &used {
        num_used_literals += u32::from(u);
    }
    choose_min_match_len(num_used_literals, max_search_depth)
}

/// `bsr32`: the index of the highest set bit of `v` (not 0).
#[inline]
fn bsr32(v: u32) -> i32 {
    31 - v.leading_zeros() as i32
}

/// Port of `recalculate_min_match_len` (external/deflate/lib/deflate_compress.c:2359-2378
/// @ libdeflate v1.26): [`choose_min_match_len`] from the literals the block uses, now that
/// their distribution is known (literals used very rarely are ignored).
fn recalculate_min_match_len(freqs: &Freqs, max_search_depth: u32) -> u32 {
    let mut literal_freq = 0u32;
    let mut num_used_literals = 0u32;

    for &f in &freqs.litlen[..DEFLATE_NUM_LITERALS] {
        literal_freq = literal_freq.wrapping_add(f);
    }

    let cutoff = literal_freq >> 10; // Ignore literals used very rarely.

    for &f in &freqs.litlen[..DEFLATE_NUM_LITERALS] {
        if f > cutoff {
            num_used_literals += 1;
        }
    }
    choose_min_match_len(num_used_literals, max_search_depth)
}

/// Port of `choose_max_block_end`(external/deflate/lib/deflate_compress.c:2380-2386
/// @ libdeflate v1.26).
#[inline]
fn choose_max_block_end(in_block_begin: usize, in_end: usize, soft_max_len: usize) -> usize {
    if in_end - in_block_begin < soft_max_len + MIN_BLOCK_LENGTH {
        return in_end;
    }
    in_block_begin + soft_max_len
}

/// Port of `deflate_compress_none` (external/deflate/lib/deflate_compress.c:2392-2443
/// @ libdeflate v1.26): the level 0 "compressor", which always outputs uncompressed blocks.
/// Returns the size written, or 0 when `out` is too small.
fn deflate_compress_none(input: &[u8], out: &mut [u8]) -> usize {
    let in_end = input.len();
    let out_end = out.len();
    let mut in_next = 0usize;
    let mut out_next = 0usize;

    // If the input is zero-length, we still must output a block in order for the output to be
    // a valid DEFLATE stream.
    if input.is_empty() {
        if out.len() < 5 {
            return 0;
        }
        // BFINAL and BTYPE
        out[0] = 1 | ((DEFLATE_BLOCKTYPE_UNCOMPRESSED as u8) << 1);
        // LEN and NLEN
        out[1..5].copy_from_slice(&0xFFFF0000u32.to_le_bytes());
        return 5;
    }

    loop {
        let mut bfinal = 0u8;
        let mut len = u16::MAX as usize;

        if in_end - in_next <= u16::MAX as usize {
            bfinal = 1;
            len = in_end - in_next;
        }
        if out_end - out_next < 5 + len {
            return 0;
        }
        // Output BFINAL and BTYPE. The stream is already byte-aligned here, so this step
        // always requires outputting exactly 1 byte.
        out[out_next] = bfinal | ((DEFLATE_BLOCKTYPE_UNCOMPRESSED as u8) << 1);
        out_next += 1;

        // Output LEN and NLEN, then the data itself.
        out[out_next..out_next + 2].copy_from_slice(&(len as u16).to_le_bytes());
        out_next += 2;
        out[out_next..out_next + 2].copy_from_slice(&(!(len as u16)).to_le_bytes());
        out_next += 2;
        out[out_next..out_next + len].copy_from_slice(&input[in_next..in_next + len]);
        out_next += len;
        in_next += len;
        if in_next == in_end {
            break;
        }
    }

    out_next
}

impl BlockSplitStats {
    /// Port of `init_block_split_stats` (external/deflate/lib/deflate_compress.c:2092-2103
    /// @ libdeflate v1.26): the statistics of a new block.
    fn init(&mut self) {
        *self = BlockSplitStats::default();
    }

    /// Port of `observe_literal` (external/deflate/lib/deflate_compress.c:2109-2114
    /// @ libdeflate v1.26): the top 2 bits and low 1 bit of the literal, for 8 possible
    /// literal observation types.
    #[inline]
    fn observe_literal(&mut self, lit: u8) {
        self.new_observations[(((lit >> 5) & 0x6) | (lit & 1)) as usize] += 1;
        self.num_new_observations += 1;
    }

    /// Port of `observe_match` (external/deflate/lib/deflate_compress.c:2120-2126 @ libdeflate
    /// v1.26): one observation type for "short match" and one for "long match".
    #[inline]
    fn observe_match(&mut self, length: u32) {
        self.new_observations[NUM_LITERAL_OBSERVATION_TYPES + usize::from(length >= 9)] += 1;
        self.num_new_observations += 1;
    }

    /// Port of `merge_new_observations` (external/deflate/lib/deflate_compress.c:2128-2139
    /// @ libdeflate v1.26).
    fn merge_new_observations(&mut self) {
        for i in 0..NUM_OBSERVATION_TYPES {
            self.observations[i] = self.observations[i].wrapping_add(self.new_observations[i]);
            self.new_observations[i] = 0;
        }
        self.num_observations = self
            .num_observations
            .wrapping_add(self.num_new_observations);
        self.num_new_observations = 0;
    }

    /// Port of `do_end_block_check` (external/deflate/lib/deflate_compress.c:2141-2197
    /// @ libdeflate v1.26): whether the new observations differ enough from the block's to end
    /// it; if not, they join the block's.
    fn do_end_block_check(&mut self, block_length: u32) -> bool {
        if self.num_observations > 0 {
            // Compute the sum of absolute differences of probabilities. To avoid needing to
            // use floating point arithmetic or do slow divisions, we do all arithmetic with
            // the probabilities multiplied by num_observations * num_new_observations.
            let mut total_delta = 0u32;
            for i in 0..NUM_OBSERVATION_TYPES {
                let expected = self.observations[i].wrapping_mul(self.num_new_observations);
                let actual = self.new_observations[i].wrapping_mul(self.num_observations);
                let delta = actual.abs_diff(expected);
                total_delta = total_delta.wrapping_add(delta);
            }

            let num_items = self
                .num_observations
                .wrapping_add(self.num_new_observations);
            // Heuristic: the cutoff is when the sum of absolute differences of probabilities
            // becomes at least 200/512. As above, the probability is multiplied by both
            // num_new_observations and num_observations. Be careful to avoid integer
            // overflow.
            let mut cutoff = (self.num_new_observations.wrapping_mul(200) / 512)
                .wrapping_mul(self.num_observations);
            // Very short blocks have a lot of overhead for the Huffman codes, so only use them
            // if it clearly seems worthwhile. (This is an additional penalty, which adds to
            // the smaller penalty below which scales more slowly.)
            if block_length < 10000 && num_items < 8192 {
                cutoff = cutoff
                    .wrapping_add((u64::from(cutoff) * u64::from(8192 - num_items) / 8192) as u32);
            }

            // Ready to end the block?
            if total_delta.wrapping_add((block_length / 4096).wrapping_mul(self.num_observations))
                >= cutoff
            {
                return true;
            }
        }
        self.merge_new_observations();
        false
    }

    /// Port of `ready_to_check_block` (external/deflate/lib/deflate_compress.c:2199-2207
    /// @ libdeflate v1.26).
    #[inline]
    fn ready_to_check_block(&self, in_block_begin: usize, in_next: usize, in_end: usize) -> bool {
        self.num_new_observations >= NUM_OBSERVATIONS_PER_BLOCK_CHECK
            && in_next - in_block_begin >= MIN_BLOCK_LENGTH
            && in_end - in_next >= MIN_BLOCK_LENGTH
    }

    /// Port of `should_end_block` (external/deflate/lib/deflate_compress.c:2209-2218
    /// @ libdeflate v1.26).
    #[inline]
    fn should_end_block(&mut self, in_block_begin: usize, in_next: usize, in_end: usize) -> bool {
        // Ready to try to end the block (again)?
        if !self.ready_to_check_block(in_block_begin, in_next, in_end) {
            return false;
        }
        self.do_end_block_check((in_next - in_block_begin) as u32)
    }
}

impl Compressor {
    /// Port of `libdeflate_alloc_compressor` and `libdeflate_alloc_compressor_ex`
    /// (external/deflate/lib/deflate_compress.c:3872-4028 @ libdeflate v1.26): a compressor
    /// for `compression_level`, from 0 (no compression) to 12, or -1 for the default (6);
    /// `None` for another level.
    pub fn new(compression_level: i32) -> Option<Compressor> {
        // Note: For similarity with zlib's API, -1 is accepted as an alias for the default
        // compression level.
        let compression_level = if compression_level == -1 {
            6
        } else {
            compression_level
        };
        if !(0..=12).contains(&compression_level) {
            return None;
        }

        let mut c = Compressor {
            strategy: Strategy::None,
            compression_level: compression_level as u32,
            // The higher the compression level, the more we should bother trying to compress
            // very small inputs.
            max_passthrough_size: (55 - (compression_level * 4)) as usize,
            max_search_depth: 0,
            nice_match_length: 0,
            freqs: Freqs {
                litlen: [0; DEFLATE_NUM_LITLEN_SYMS],
                offset: [0; DEFLATE_NUM_OFFSET_SYMS],
            },
            split_stats: BlockSplitStats::default(),
            codes: Codes::new(),
            static_codes: Codes::new(),
            precode: Box::new(Precode {
                freqs: [0; DEFLATE_NUM_PRECODE_SYMS],
                codewords: [0; DEFLATE_NUM_PRECODE_SYMS],
                lens: [0; DEFLATE_NUM_PRECODE_SYMS],
                items: [0; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS],
                num_litlen_syms: 0,
                num_offset_syms: 0,
                num_explicit_lens: 0,
                num_items: 0,
            }),
            length: Box::new(LengthCodes {
                codewords: [0; DEFLATE_MAX_MATCH_LEN + 1],
                lens: [0; DEFLATE_MAX_MATCH_LEN + 1],
            }),
            hc_mf: None,
            ht_mf: None,
            sequences: Vec::new(),
            n: None,
        };

        let (strategy, depth, nice) = match compression_level {
            0 => {
                c.max_passthrough_size = usize::MAX;
                (Strategy::None, 0, 0)
            }
            // max_search_depth is unused.
            1 => (Strategy::Fastest, 0, 32),
            2 => (Strategy::Greedy, 6, 10),
            3 => (Strategy::Greedy, 12, 14),
            4 => (Strategy::Greedy, 16, 30),
            5 => (Strategy::Lazy, 16, 30),
            6 => (Strategy::Lazy, 35, 65),
            7 => (Strategy::Lazy, 100, 130),
            8 => (Strategy::Lazy2, 300, DEFLATE_MAX_MATCH_LEN as u32),
            9 => (Strategy::Lazy2, 600, DEFLATE_MAX_MATCH_LEN as u32),
            10 => {
                c.n = Some(NearOptimal::new(2, 32, 32, 0));
                (Strategy::NearOptimal, 35, 75)
            }
            11 => {
                c.n = Some(NearOptimal::new(4, 16, 16, 1000));
                (Strategy::NearOptimal, 100, 150)
            }
            // 12 (and upstream's default:, which the range check above leaves unreachable)
            _ => {
                c.n = Some(NearOptimal::new(10, 1, 1, 10000));
                (Strategy::NearOptimal, 300, DEFLATE_MAX_MATCH_LEN as u32)
            }
        };
        c.strategy = strategy;
        c.max_search_depth = depth;
        c.nice_match_length = nice;
        if matches!(
            strategy,
            Strategy::Greedy | Strategy::Lazy | Strategy::Lazy2
        ) {
            c.hc_mf = Some(HcMatchfinder::new());
            c.sequences = vec![Sequence::default(); SEQ_STORE_LENGTH + 1];
        }
        if strategy == Strategy::Fastest {
            c.ht_mf = Some(HtMatchfinder::new());
            c.sequences = vec![Sequence::default(); FAST_SEQ_STORE_LENGTH + 1];
        }

        c.deflate_init_static_codes();
        Some(c)
    }

    /// Port of `libdeflate_get_compression_level` (external/deflate/lib/deflate_compress.c:4081-4085
    /// @ libdeflate v1.26).
    pub fn compression_level(&self) -> u32 {
        self.compression_level
    }

    /// Port of `deflate_reset_symbol_frequencies` (external/deflate/lib/deflate_compress.c:1402-1406
    /// @ libdeflate v1.26).
    fn deflate_reset_symbol_frequencies(&mut self) {
        self.freqs.litlen = [0; DEFLATE_NUM_LITLEN_SYMS];
        self.freqs.offset = [0; DEFLATE_NUM_OFFSET_SYMS];
    }

    /// Port of `deflate_init_static_codes` (external/deflate/lib/deflate_compress.c:1432-1450
    /// @ libdeflate v1.26): the static codes, as the codes of frequencies that give the
    /// format's lengths.
    fn deflate_init_static_codes(&mut self) {
        let mut i = 0;
        while i < 144 {
            self.freqs.litlen[i] = 1 << (9 - 8);
            i += 1;
        }
        while i < 256 {
            self.freqs.litlen[i] = 1; // 1 << (9 - 9)
            i += 1;
        }
        while i < 280 {
            self.freqs.litlen[i] = 1 << (9 - 7);
            i += 1;
        }
        while i < 288 {
            self.freqs.litlen[i] = 1 << (9 - 8);
            i += 1;
        }
        self.freqs.offset.fill(1); // 1 << (5 - 5)
        deflate_make_huffman_codes(&self.freqs, &mut self.static_codes);
    }

    /// Port of `deflate_precompute_huffman_header` (external/deflate/lib/deflate_compress.c:1570-1631
    /// @ libdeflate v1.26): the precode items and the precode, for a dynamic block.
    fn deflate_precompute_huffman_header(&mut self) {
        let p = &mut *self.precode;
        let lens = &mut self.codes.lens;

        // Compute how many litlen and offset symbols are needed.
        p.num_litlen_syms = DEFLATE_NUM_LITLEN_SYMS;
        while p.num_litlen_syms > 257 {
            if lens[p.num_litlen_syms - 1] != 0 {
                break;
            }
            p.num_litlen_syms -= 1;
        }
        p.num_offset_syms = DEFLATE_NUM_OFFSET_SYMS;
        while p.num_offset_syms > 1 {
            if lens[LENS_OFFSET + p.num_offset_syms - 1] != 0 {
                break;
            }
            p.num_offset_syms -= 1;
        }

        // If we're not using the full set of literal/length codeword lengths, then temporarily
        // move the offset codeword lengths over so that the literal/length and offset
        // codeword lengths are contiguous.
        if p.num_litlen_syms != DEFLATE_NUM_LITLEN_SYMS {
            lens.copy_within(
                LENS_OFFSET..LENS_OFFSET + p.num_offset_syms,
                p.num_litlen_syms,
            );
        }

        // Compute the "items" (RLE / literal tokens and extra bits) with which the codeword
        // lengths in the larger code will be output.
        p.num_items = deflate_compute_precode_items(
            &lens[..p.num_litlen_syms + p.num_offset_syms],
            &mut p.freqs,
            &mut p.items,
        );

        // Build the precode.
        deflate_make_huffman_code(
            DEFLATE_NUM_PRECODE_SYMS,
            MAX_PRE_CODEWORD_LEN,
            &p.freqs,
            &mut p.lens,
            &mut p.codewords,
        );

        // Count how many precode lengths we actually need to output.
        p.num_explicit_lens = DEFLATE_NUM_PRECODE_SYMS;
        while p.num_explicit_lens > 4 {
            if p.lens[DEFLATE_PRECODE_LENS_PERMUTATION[p.num_explicit_lens - 1] as usize] != 0 {
                break;
            }
            p.num_explicit_lens -= 1;
        }

        // Restore the offset codeword lengths if needed.
        if p.num_litlen_syms != DEFLATE_NUM_LITLEN_SYMS {
            lens.copy_within(
                p.num_litlen_syms..p.num_litlen_syms + p.num_offset_syms,
                LENS_OFFSET,
            );
        }
    }

    /// Port of `deflate_compute_full_len_codewords` (external/deflate/lib/deflate_compress.c:1638-1658
    /// @ libdeflate v1.26): the litlen codeword of each match length with its extra bits
    /// appended, for `codes`.
    fn deflate_compute_full_len_codewords(length: &mut LengthCodes, codes: &Codes) {
        for len in DEFLATE_MIN_MATCH_LEN..=DEFLATE_MAX_MATCH_LEN {
            let slot = DEFLATE_LENGTH_SLOT[len] as usize;
            let litlen_sym = DEFLATE_FIRST_LEN_SYM + slot;
            let extra_bits = len as u32 - DEFLATE_LENGTH_SLOT_BASE[slot];

            length.codewords[len] =
                codes.codewords_litlen[litlen_sym] | (extra_bits << codes.lens[litlen_sym]);
            length.lens[len] = codes.lens[litlen_sym] + DEFLATE_EXTRA_LENGTH_BITS[slot];
        }
    }

    /// Port of `deflate_flush_block` (external/deflate/lib/deflate_compress.c:1706-2038
    /// @ libdeflate v1.26): outputs the block `data[block_begin..block_begin + block_length]`
    /// with the cheapest of the uncompressed, static and dynamic block types (on a tie,
    /// uncompressed, then static), from its `items`; or sets the overflow flag when the
    /// block won't fit.
    fn deflate_flush_block(
        &mut self,
        os: &mut OutputBitstream<'_>,
        data: &[u8],
        block_begin: usize,
        block_length: u32,
        items: Items<'_>,
        is_final_block: bool,
    ) {
        let mut in_next = block_begin;
        let in_end = block_begin + block_length as usize;
        let end = os.out.len();
        let out_fast_end = end - (WORDBYTES - 1).min(end - os.next);

        // The cost for each block type, in bits. Start with the cost of the block header which
        // is 3 bits.
        let mut dynamic_cost: u32 = 3;
        let mut static_cost: u32 = 3;
        let mut uncompressed_cost: u32 = 3;

        // Precompute the precode items and build the precode.
        self.deflate_precompute_huffman_header();
        let p = &*self.precode;

        // Account for the cost of encoding dynamic Huffman codes.
        dynamic_cost += 5 + 5 + 4 + (3 * p.num_explicit_lens as u32);
        for sym in 0..DEFLATE_NUM_PRECODE_SYMS {
            let extra = u32::from(DEFLATE_EXTRA_PRECODE_BITS[sym]);
            dynamic_cost = dynamic_cost
                .wrapping_add(p.freqs[sym].wrapping_mul(extra + u32::from(p.lens[sym])));
        }

        // Account for the cost of encoding literals.
        let lens = &self.codes.lens;
        let f = &self.freqs;
        for sym in 0..144 {
            dynamic_cost =
                dynamic_cost.wrapping_add(f.litlen[sym].wrapping_mul(u32::from(lens[sym])));
            static_cost = static_cost.wrapping_add(f.litlen[sym].wrapping_mul(8));
        }
        for sym in 144..256 {
            dynamic_cost =
                dynamic_cost.wrapping_add(f.litlen[sym].wrapping_mul(u32::from(lens[sym])));
            static_cost = static_cost.wrapping_add(f.litlen[sym].wrapping_mul(9));
        }

        // Account for the cost of encoding the end-of-block symbol.
        dynamic_cost = dynamic_cost.wrapping_add(u32::from(lens[DEFLATE_END_OF_BLOCK]));
        static_cost = static_cost.wrapping_add(7);

        // Account for the cost of encoding lengths.
        for sym in DEFLATE_FIRST_LEN_SYM..DEFLATE_FIRST_LEN_SYM + DEFLATE_EXTRA_LENGTH_BITS.len() {
            let extra = u32::from(DEFLATE_EXTRA_LENGTH_BITS[sym - DEFLATE_FIRST_LEN_SYM]);
            dynamic_cost =
                dynamic_cost.wrapping_add(f.litlen[sym].wrapping_mul(extra + u32::from(lens[sym])));
            static_cost = static_cost.wrapping_add(
                f.litlen[sym].wrapping_mul(extra + u32::from(self.static_codes.lens[sym])),
            );
        }

        // Account for the cost of encoding offsets.
        for sym in 0..DEFLATE_EXTRA_OFFSET_BITS.len() {
            let extra = u32::from(DEFLATE_EXTRA_OFFSET_BITS[sym]);
            dynamic_cost = dynamic_cost.wrapping_add(
                f.offset[sym].wrapping_mul(extra + u32::from(lens[LENS_OFFSET + sym])),
            );
            static_cost = static_cost.wrapping_add(f.offset[sym].wrapping_mul(extra + 5));
        }

        // Compute the cost of using uncompressed blocks.
        uncompressed_cost = uncompressed_cost.wrapping_add(
            (0u32.wrapping_sub(os.bitcount + 3) & 7)
                + 32
                + (40 * ((block_length.div_ceil(u16::MAX as u32)) - 1))
                + (8 * block_length),
        );

        // Choose and output the cheapest type of block. If there is a tie, prefer
        // uncompressed, then static, then dynamic.
        let best_cost = dynamic_cost.min(static_cost.min(uncompressed_cost));

        // If the block isn't going to fit, then stop early.
        if ((os.bitcount + best_cost).div_ceil(8) as usize) > end - os.next {
            os.overflow = true;
            return;
        }
        // Else, now we know that the block fits, so no further bounds checks on the output
        // buffer are required until the next block.

        if best_cost == uncompressed_cost {
            // Uncompressed block(s). DEFLATE limits the length of uncompressed blocks to
            // UINT16_MAX bytes, so if the length of the "block" we're flushing is over
            // UINT16_MAX, we actually output multiple blocks.
            loop {
                let mut bfinal = 0u8;
                let mut len = u16::MAX as usize;

                if in_end - in_next <= u16::MAX as usize {
                    bfinal = u8::from(is_final_block);
                    len = in_end - in_next;
                }
                // Output BFINAL (1 bit) and BTYPE (2 bits), then align to a byte boundary.
                os.out[os.next] = (bfinal << os.bitcount) | os.bitbuf as u8;
                os.next += 1;
                if os.bitcount > 5 {
                    os.out[os.next] = 0;
                    os.next += 1;
                }
                os.bitbuf = 0;
                os.bitcount = 0;
                // Output LEN and NLEN, then the data itself.
                let n = os.next;
                os.out[n..n + 2].copy_from_slice(&(len as u16).to_le_bytes());
                os.out[n + 2..n + 4].copy_from_slice(&(!(len as u16)).to_le_bytes());
                os.out[n + 4..n + 4 + len].copy_from_slice(&data[in_next..in_next + len]);
                os.next = n + 4 + len;
                in_next += len;
                if in_next == in_end {
                    break;
                }
            }
            // Done outputting uncompressed block(s)
            return;
        }

        let use_static = best_cost == static_cost;
        if use_static {
            // Static Huffman block
            os.add_bits(u32::from(is_final_block), 1);
            os.add_bits(DEFLATE_BLOCKTYPE_STATIC_HUFFMAN, 2);
            os.flush_bits(out_fast_end);
        } else {
            let num_explicit_lens = p.num_explicit_lens;
            let num_precode_items = p.num_items;

            // Dynamic Huffman block
            os.add_bits(u32::from(is_final_block), 1);
            os.add_bits(DEFLATE_BLOCKTYPE_DYNAMIC_HUFFMAN, 2);
            os.add_bits((p.num_litlen_syms - 257) as u32, 5);
            os.add_bits((p.num_offset_syms - 1) as u32, 5);
            os.add_bits((num_explicit_lens - 4) as u32, 4);

            // Output the lengths of the codewords in the precode.
            if can_buffer(3 * (DEFLATE_NUM_PRECODE_SYMS as u32 - 1)) {
                // A 64-bit bitbuffer is just one bit too small to hold the maximum number of
                // precode lens, so to minimize flushes we merge one len with the previous
                // fields.
                let precode_sym = DEFLATE_PRECODE_LENS_PERMUTATION[0] as usize;
                os.add_bits(u32::from(p.lens[precode_sym]), 3);
                os.flush_bits(out_fast_end);
                // num_explicit_lens >= 4
                for &sym in &DEFLATE_PRECODE_LENS_PERMUTATION[1..num_explicit_lens] {
                    os.add_bits(u32::from(p.lens[sym as usize]), 3);
                }
                os.flush_bits(out_fast_end);
            } else {
                os.flush_bits(out_fast_end);
                for &sym in &DEFLATE_PRECODE_LENS_PERMUTATION[..num_explicit_lens] {
                    os.add_bits(u32::from(p.lens[sym as usize]), 3);
                    os.flush_bits(out_fast_end);
                }
            }

            // Output the lengths of the codewords in the litlen and offset codes, encoded by
            // the precode.
            for &precode_item in &p.items[..num_precode_items] {
                let precode_sym = (precode_item & 0x1F) as usize;
                os.add_bits(p.codewords[precode_sym], u32::from(p.lens[precode_sym]));
                os.add_bits(
                    precode_item >> 5,
                    u32::from(DEFLATE_EXTRA_PRECODE_BITS[precode_sym]),
                );
                os.flush_bits(out_fast_end);
            }
        }

        // Output the literals and matches for a dynamic or static block.
        let codes = if use_static {
            &self.static_codes
        } else {
            &self.codes
        };
        Self::deflate_compute_full_len_codewords(&mut self.length, codes);
        let length = &*self.length;

        let sequences = match items {
            Items::Optimum(n) => {
                // Output the literals and matches from the minimum-cost path
                let end_node = block_length as usize;
                let mut cur_node = 0usize;
                loop {
                    let item = n.optimum_nodes[cur_node].item;
                    let length_ = item & OPTIMUM_LEN_MASK;
                    let offset = item >> OPTIMUM_OFFSET_SHIFT;

                    if length_ == 1 {
                        // Literal
                        os.add_bits(
                            codes.codewords_litlen[offset as usize],
                            u32::from(codes.lens[offset as usize]),
                        );
                        os.flush_bits(out_fast_end);
                    } else {
                        // Match
                        write_match(
                            os,
                            out_fast_end,
                            length,
                            codes,
                            length_,
                            offset,
                            u32::from(n.offset_slot_full[offset as usize]),
                        );
                    }
                    cur_node += length_ as usize;
                    if cur_node == end_node {
                        break;
                    }
                }
                &[][..]
            }
            Items::Sequences(s) => s,
        };

        // Output the literals and matches from the sequences list.
        let lit = |os: &mut OutputBitstream<'_>, in_next: &mut usize| {
            let l = data[*in_next] as usize;
            *in_next += 1;
            os.add_bits(codes.codewords_litlen[l], u32::from(codes.lens[l]));
        };
        for seq in sequences {
            let mut litrunlen = seq.litrunlen_and_length & SEQ_LITRUNLEN_MASK;
            let length_ = seq.litrunlen_and_length >> SEQ_LENGTH_SHIFT;

            // Output a run of literals.
            // (CAN_BUFFER(4 * MAX_LITLEN_CODEWORD_LEN) holds for a 64-bit bit buffer.)
            while litrunlen >= 4 {
                lit(os, &mut in_next);
                lit(os, &mut in_next);
                lit(os, &mut in_next);
                lit(os, &mut in_next);
                os.flush_bits(out_fast_end);
                litrunlen -= 4;
            }
            if litrunlen != 0 {
                lit(os, &mut in_next);
                if litrunlen > 1 {
                    lit(os, &mut in_next);
                    if litrunlen > 2 {
                        lit(os, &mut in_next);
                    }
                }
                os.flush_bits(out_fast_end);
            }

            if length_ == 0 {
                // Last sequence?
                break;
            }

            // Output a match.
            write_match(
                os,
                out_fast_end,
                length,
                codes,
                length_,
                u32::from(seq.offset),
                u32::from(seq.offset_slot),
            );
            in_next += length_ as usize;
        }

        // Output the end-of-block symbol.
        os.add_bits(
            codes.codewords_litlen[DEFLATE_END_OF_BLOCK],
            u32::from(codes.lens[DEFLATE_END_OF_BLOCK]),
        );
        os.flush_bits(out_fast_end);
    }

    /// Port of `deflate_finish_block` (external/deflate/lib/deflate_compress.c:2040-2051
    /// @ libdeflate v1.26): the block's Huffman codes, then its output.
    fn deflate_finish_block(
        &mut self,
        os: &mut OutputBitstream<'_>,
        data: &[u8],
        block_begin: usize,
        block_length: u32,
        num_sequences: usize,
        is_final_block: bool,
    ) {
        self.freqs.litlen[DEFLATE_END_OF_BLOCK] += 1;
        deflate_make_huffman_codes(&self.freqs, &mut self.codes);
        let sequences = std::mem::take(&mut self.sequences);
        self.deflate_flush_block(
            os,
            data,
            block_begin,
            block_length,
            Items::Sequences(&sequences[..=num_sequences]),
            is_final_block,
        );
        self.sequences = sequences;
    }

    /// Port of `deflate_begin_sequences` (external/deflate/lib/deflate_compress.c:2222-2228
    /// @ libdeflate v1.26).
    fn deflate_begin_sequences(&mut self, first_seq: usize) {
        self.deflate_reset_symbol_frequencies();
        self.sequences[first_seq].litrunlen_and_length = 0;
    }

    /// Port of `deflate_choose_literal` (external/deflate/lib/deflate_compress.c:2230-2241
    /// @ libdeflate v1.26).
    #[inline]
    fn deflate_choose_literal(&mut self, literal: u8, gather_split_stats: bool, seq: usize) {
        self.freqs.litlen[literal as usize] += 1;
        if gather_split_stats {
            self.split_stats.observe_literal(literal);
        }
        self.sequences[seq].litrunlen_and_length += 1;
    }

    /// Port of `deflate_choose_match` (external/deflate/lib/deflate_compress.c:2243-2264
    /// @ libdeflate v1.26): ends the current sequence with the match, and starts the next.
    #[inline]
    fn deflate_choose_match(
        &mut self,
        length: u32,
        offset: u32,
        gather_split_stats: bool,
        seq_p: &mut usize,
    ) {
        let seq = *seq_p;
        let length_slot = DEFLATE_LENGTH_SLOT[length as usize] as usize;
        let offset_slot = deflate_get_offset_slot(offset);

        self.freqs.litlen[DEFLATE_FIRST_LEN_SYM + length_slot] += 1;
        self.freqs.offset[offset_slot as usize] += 1;
        if gather_split_stats {
            self.split_stats.observe_match(length);
        }

        let s = &mut self.sequences[seq];
        s.litrunlen_and_length |= length << SEQ_LENGTH_SHIFT;
        s.offset = offset as u16;
        s.offset_slot = offset_slot as u16;

        self.sequences[seq + 1].litrunlen_and_length = 0;
        *seq_p = seq + 1;
    }

    /// Port of `deflate_compress_fastest` (external/deflate/lib/deflate_compress.c:2451-2523
    /// @ libdeflate v1.26): a faster variant of the greedy compressor, on `ht_matchfinder`,
    /// with fixed-length blocks (no block splitting).
    fn deflate_compress_fastest(&mut self, data: &[u8], os: &mut OutputBitstream<'_>) {
        let in_end = data.len();
        let mut in_next = 0usize;
        let mut in_cur_base = 0usize;
        let mut max_len = DEFLATE_MAX_MATCH_LEN as u32;
        let mut nice_len = self.nice_match_length.min(max_len);
        let mut next_hash = 0u32;
        let mut mf = self.ht_mf.take().expect("the fastest parser's matchfinder");

        mf.init();

        loop {
            // Starting a new DEFLATE block
            let in_block_begin = in_next;
            let in_max_block_end =
                choose_max_block_end(in_next, in_end, FAST_SOFT_MAX_BLOCK_LENGTH);
            let mut seq = 0usize;

            self.deflate_begin_sequences(seq);

            loop {
                let mut offset = 0u32;
                let remaining = in_end - in_next;

                if remaining < DEFLATE_MAX_MATCH_LEN {
                    max_len = remaining as u32;
                    if max_len < HT_MATCHFINDER_REQUIRED_NBYTES {
                        loop {
                            self.deflate_choose_literal(data[in_next], false, seq);
                            in_next += 1;
                            max_len -= 1;
                            if max_len == 0 {
                                break;
                            }
                        }
                        break;
                    }
                    nice_len = nice_len.min(max_len);
                }
                let length = mf.longest_match(
                    data,
                    &mut in_cur_base,
                    in_next,
                    max_len,
                    nice_len,
                    &mut next_hash,
                    &mut offset,
                );
                if length != 0 {
                    // Match found
                    self.deflate_choose_match(length, offset, false, &mut seq);
                    mf.skip_bytes(
                        data,
                        &mut in_cur_base,
                        in_next + 1,
                        in_end,
                        length - 1,
                        &mut next_hash,
                    );
                    in_next += length as usize;
                } else {
                    // No match found
                    self.deflate_choose_literal(data[in_next], false, seq);
                    in_next += 1;
                }

                // Check if it's time to output another block.
                if !(in_next < in_max_block_end && seq < FAST_SEQ_STORE_LENGTH) {
                    break;
                }
            }

            self.deflate_finish_block(
                os,
                data,
                in_block_begin,
                (in_next - in_block_begin) as u32,
                seq,
                in_next == in_end,
            );
            if in_next == in_end || os.overflow {
                break;
            }
        }
        self.ht_mf = Some(mf);
    }

    /// Port of `deflate_compress_greedy`(external/deflate/lib/deflate_compress.c:2528-2602
    /// @ libdeflate v1.26): the "greedy" DEFLATE compressor, which always chooses the longest
    /// match.
    fn deflate_compress_greedy(&mut self, data: &[u8], os: &mut OutputBitstream<'_>) {
        let in_end = data.len();
        let mut in_next = 0usize;
        let mut in_cur_base = 0usize;
        let mut max_len = DEFLATE_MAX_MATCH_LEN as u32;
        let mut nice_len = self.nice_match_length.min(max_len);
        let mut next_hashes = [0u32; 2];
        let mut mf = self.hc_mf.take().expect("the greedy parser's matchfinder");

        mf.init();

        loop {
            // Starting a new DEFLATE block
            let in_block_begin = in_next;
            let in_max_block_end = choose_max_block_end(in_next, in_end, SOFT_MAX_BLOCK_LENGTH);
            let mut seq = 0usize;

            self.split_stats.init();
            self.deflate_begin_sequences(seq);
            let min_len =
                calculate_min_match_len(&data[in_next..in_max_block_end], self.max_search_depth);
            loop {
                let mut offset = 0u32;

                adjust_max_and_nice_len(&mut max_len, &mut nice_len, in_end - in_next);
                let length = mf.longest_match(
                    data,
                    &mut in_cur_base,
                    in_next,
                    min_len - 1,
                    max_len,
                    nice_len,
                    self.max_search_depth,
                    &mut next_hashes,
                    &mut offset,
                );

                if length >= min_len && (length > DEFLATE_MIN_MATCH_LEN as u32 || offset <= 4096) {
                    // Match found
                    self.deflate_choose_match(length, offset, true, &mut seq);
                    mf.skip_bytes(
                        data,
                        &mut in_cur_base,
                        in_next + 1,
                        in_end,
                        length - 1,
                        &mut next_hashes,
                    );
                    in_next += length as usize;
                } else {
                    // No match found
                    self.deflate_choose_literal(data[in_next], true, seq);
                    in_next += 1;
                }

                // Check if it's time to output another block.
                if !(in_next < in_max_block_end
                    && seq < SEQ_STORE_LENGTH
                    && !self
                        .split_stats
                        .should_end_block(in_block_begin, in_next, in_end))
                {
                    break;
                }
            }

            self.deflate_finish_block(
                os,
                data,
                in_block_begin,
                (in_next - in_block_begin) as u32,
                seq,
                in_next == in_end,
            );
            if in_next == in_end || os.overflow {
                break;
            }
        }
        self.hc_mf = Some(mf);
    }

    /// Port of `deflate_compress_lazy_generic`, `deflate_compress_lazy` and
    /// `deflate_compress_lazy2` (external/deflate/lib/deflate_compress.c:2604-2834 @ libdeflate
    /// v1.26): before choosing a match, look for a better one at the next position (and, in
    /// `lazy2` mode, the one after); if there is one, output a literal and go on from there.
    /// `LAZY2` is a constant, as upstream's `forceinline` makes it in each caller.
    fn deflate_compress_lazy_generic<const LAZY2: bool>(
        &mut self,
        data: &[u8],
        os: &mut OutputBitstream<'_>,
    ) {
        let in_end = data.len();
        let mut in_next = 0usize;
        let mut in_cur_base = 0usize;
        let mut max_len = DEFLATE_MAX_MATCH_LEN as u32;
        let mut nice_len = self.nice_match_length.min(max_len);
        let mut next_hashes = [0u32; 2];
        let mut mf = self.hc_mf.take().expect("the lazy parser's matchfinder");
        let depth = self.max_search_depth;

        mf.init();

        loop {
            // Starting a new DEFLATE block
            let in_block_begin = in_next;
            let in_max_block_end = choose_max_block_end(in_next, in_end, SOFT_MAX_BLOCK_LENGTH);
            let mut next_recalc_min_len = in_next + (in_end - in_next).min(10000);
            let mut seq = 0usize;

            self.split_stats.init();
            self.deflate_begin_sequences(seq);
            let mut min_len = calculate_min_match_len(&data[in_next..in_max_block_end], depth);
            loop {
                // (upstream's `continue`s go to the check at the end of this loop)
                'step: {
                    // Recalculate the minimum match length if it hasn't been done recently.
                    if in_next >= next_recalc_min_len {
                        min_len = recalculate_min_match_len(&self.freqs, depth);
                        next_recalc_min_len +=
                            (in_end - next_recalc_min_len).min(in_next - in_block_begin);
                    }

                    // Find the longest match at the current position.
                    adjust_max_and_nice_len(&mut max_len, &mut nice_len, in_end - in_next);
                    let mut cur_offset = 0u32;
                    let mut cur_len = mf.longest_match(
                        data,
                        &mut in_cur_base,
                        in_next,
                        min_len - 1,
                        max_len,
                        nice_len,
                        depth,
                        &mut next_hashes,
                        &mut cur_offset,
                    );
                    if cur_len < min_len
                        || (cur_len == DEFLATE_MIN_MATCH_LEN as u32 && cur_offset > 8192)
                    {
                        // No match found. Choose a literal.
                        self.deflate_choose_literal(data[in_next], true, seq);
                        in_next += 1;
                        break 'step;
                    }
                    in_next += 1;

                    // have_cur_match:
                    loop {
                        // We have a match at the current position. If it's very long, choose
                        // it immediately.
                        if cur_len >= nice_len {
                            self.deflate_choose_match(cur_len, cur_offset, true, &mut seq);
                            mf.skip_bytes(
                                data,
                                &mut in_cur_base,
                                in_next,
                                in_end,
                                cur_len - 1,
                                &mut next_hashes,
                            );
                            in_next += cur_len as usize - 1;
                            break 'step;
                        }

                        // Try to find a better match at the next position. (Since we already
                        // have a match at the current position, only half the search depth is
                        // used at the next position.)
                        adjust_max_and_nice_len(&mut max_len, &mut nice_len, in_end - in_next);
                        let mut next_offset = 0u32;
                        let mut next_len = mf.longest_match(
                            data,
                            &mut in_cur_base,
                            in_next,
                            cur_len - 1,
                            max_len,
                            nice_len,
                            depth >> 1,
                            &mut next_hashes,
                            &mut next_offset,
                        );
                        in_next += 1;
                        if next_len >= cur_len
                            && 4 * (next_len - cur_len) as i32
                                + (bsr32(cur_offset) - bsr32(next_offset))
                                > 2
                        {
                            // Found a better match at the next position. Output a literal.
                            // Then the next match becomes the current match.
                            self.deflate_choose_literal(data[in_next - 2], true, seq);
                            cur_len = next_len;
                            cur_offset = next_offset;
                            continue;
                        }

                        if LAZY2 {
                            // In lazy2 mode, look ahead another position
                            adjust_max_and_nice_len(&mut max_len, &mut nice_len, in_end - in_next);
                            next_len = mf.longest_match(
                                data,
                                &mut in_cur_base,
                                in_next,
                                cur_len - 1,
                                max_len,
                                nice_len,
                                depth >> 2,
                                &mut next_hashes,
                                &mut next_offset,
                            );
                            in_next += 1;
                            if next_len >= cur_len
                                && 4 * (next_len - cur_len) as i32
                                    + (bsr32(cur_offset) - bsr32(next_offset))
                                    > 6
                            {
                                // There's a much better match two positions ahead, so use two
                                // literals.
                                self.deflate_choose_literal(data[in_next - 3], true, seq);
                                self.deflate_choose_literal(data[in_next - 2], true, seq);
                                cur_len = next_len;
                                cur_offset = next_offset;
                                continue;
                            }
                            // No better match at either of the next 2 positions. Output the
                            // current match.
                            self.deflate_choose_match(cur_len, cur_offset, true, &mut seq);
                            if cur_len > 3 {
                                mf.skip_bytes(
                                    data,
                                    &mut in_cur_base,
                                    in_next,
                                    in_end,
                                    cur_len - 3,
                                    &mut next_hashes,
                                );
                                in_next += cur_len as usize - 3;
                            }
                        } else {
                            // No better match at the next position. Output the current match.
                            self.deflate_choose_match(cur_len, cur_offset, true, &mut seq);
                            mf.skip_bytes(
                                data,
                                &mut in_cur_base,
                                in_next,
                                in_end,
                                cur_len - 2,
                                &mut next_hashes,
                            );
                            in_next += cur_len as usize - 2;
                        }
                        break 'step;
                    }
                }

                // Check if it's time to output another block.
                if !(in_next < in_max_block_end
                    && seq < SEQ_STORE_LENGTH
                    && !self
                        .split_stats
                        .should_end_block(in_block_begin, in_next, in_end))
                {
                    break;
                }
            }

            self.deflate_finish_block(
                os,
                data,
                in_block_begin,
                (in_next - in_block_begin) as u32,
                seq,
                in_next == in_end,
            );
            if in_next == in_end || os.overflow {
                break;
            }
        }
        self.hc_mf = Some(mf);
    }

    /// Port of `libdeflate_deflate_compress`(external/deflate/lib/deflate_compress.c:4030-4072
    /// @ libdeflate v1.26): the DEFLATE stream of `input` into `out`; returns its size, or 0
    /// when `out` is too small.
    pub fn deflate_compress(&mut self, input: &[u8], out: &mut [u8]) -> usize {
        // For extremely short inputs, or for compression level 0, just output uncompressed
        // blocks.
        if input.len() <= self.max_passthrough_size {
            return deflate_compress_none(input, out);
        }

        // Initialize the output bitstream structure.
        let mut os = OutputBitstream {
            bitbuf: 0,
            bitcount: 0,
            out,
            next: 0,
            overflow: false,
        };

        // Call the actual compression function.
        match self.strategy {
            Strategy::None => unreachable!("level 0 passes every input through"),
            Strategy::Greedy => self.deflate_compress_greedy(input, &mut os),
            Strategy::Lazy => self.deflate_compress_lazy_generic::<false>(input, &mut os),
            Strategy::Lazy2 => self.deflate_compress_lazy_generic::<true>(input, &mut os),
            Strategy::Fastest => self.deflate_compress_fastest(input, &mut os),
            Strategy::NearOptimal => self.deflate_compress_near_optimal(input, &mut os),
        }

        // Return 0 if the output buffer is too small.
        if os.overflow {
            return 0;
        }

        // Write the final byte if needed. This can't overflow the output buffer because
        // deflate_flush_block() would have set the overflow flag if there wasn't enough space
        // remaining for the full final block.
        if os.bitcount != 0 {
            os.out[os.next] = os.bitbuf as u8;
            os.next += 1;
        }

        // Return the compressed size in bytes.
        os.next
    }
}

// The near-optimal parser (external/deflate/lib/deflate_compress.c:121-160, 168-180, 382-436,
// 538-658, 2836-3870 @ libdeflate v1.26, `SUPPORT_NEAR_OPTIMAL_PARSING`).

/// Port of `BIT_COST` (external/deflate/lib/deflate_compress.c:140 @ libdeflate v1.26): a
/// scaling factor that lets the near-optimal compressor consider fractional bit costs.
const BIT_COST: u32 = 16;

/// Port of `LITERAL_NOSTAT_BITS` (external/deflate/lib/deflate_compress.c:149 @ libdeflate
/// v1.26): the bits assumed for a literal unused in the previous optimization pass.
const LITERAL_NOSTAT_BITS: u32 = 13;
/// Port of `LENGTH_NOSTAT_BITS` (external/deflate/lib/deflate_compress.c:150 @ libdeflate
/// v1.26).
const LENGTH_NOSTAT_BITS: u32 = 13;
/// Port of `OFFSET_NOSTAT_BITS` (external/deflate/lib/deflate_compress.c:151 @ libdeflate
/// v1.26).
const OFFSET_NOSTAT_BITS: u32 = 10;

/// Port of `MATCH_CACHE_LENGTH` (external/deflate/lib/deflate_compress.c:158 @ libdeflate
/// v1.26): (slightly less than) the most matches the near-optimal compressor caches per block.
const MATCH_CACHE_LENGTH: usize = SOFT_MAX_BLOCK_LENGTH * 5;

/// Port of `MAX_MATCHES_PER_POS` (external/deflate/lib/deflate_compress.c:177-178
/// @ libdeflate v1.26): the most matches the binary trees matchfinder finds at a position.
const MAX_MATCHES_PER_POS: usize = DEFLATE_MAX_MATCH_LEN - DEFLATE_MIN_MATCH_LEN + 1;

/// Port of `MAX_BLOCK_LENGTH` (external/deflate/lib/deflate_compress.c:188-190 @ libdeflate
/// v1.26): the largest block length ever used.
const MAX_BLOCK_LENGTH: usize = {
    let a = SOFT_MAX_BLOCK_LENGTH + MIN_BLOCK_LENGTH - 1;
    let b = SOFT_MAX_BLOCK_LENGTH + 1 + DEFLATE_MAX_MATCH_LEN;
    if a > b { a } else { b }
};

/// `DEFLATE_MAX_MATCH_OFFSET` (external/deflate/lib/deflate_constants.h:18 @ libdeflate
/// v1.26).
const DEFLATE_MAX_MATCH_OFFSET: usize = 32768;

/// Port of `OPTIMUM_OFFSET_SHIFT` (external/deflate/lib/deflate_compress.c:430 @ libdeflate
/// v1.26).
const OPTIMUM_OFFSET_SHIFT: u32 = 9;
/// Port of `OPTIMUM_LEN_MASK` (external/deflate/lib/deflate_compress.c:431 @ libdeflate v1.26).
const OPTIMUM_LEN_MASK: u32 = (1 << OPTIMUM_OFFSET_SHIFT) - 1;

/// Port of `struct deflate_costs` (external/deflate/lib/deflate_compress.c:385-395
/// @ libdeflate v1.26): the costs for the near-optimal parsing algorithm, of each literal,
/// each match length, and a match offset of each offset slot.
#[derive(Clone)]
struct Costs {
    literal: [u32; DEFLATE_NUM_LITERALS],
    length: [u32; DEFLATE_MAX_MATCH_LEN + 1],
    offset_slot: [u32; DEFLATE_NUM_OFFSET_SYMS],
}

/// Port of `struct deflate_optimum_node` (external/deflate/lib/deflate_compress.c:417-434
/// @ libdeflate v1.26): a position of the block, as a node of the graph of literal and match
/// choices: the minimum cost to reach the end of the block from it, and the item (literal or
/// match) that begins that path. The low bits of `item` are the length (1 for a literal), the
/// high bits the literal byte or the match offset.
#[derive(Clone, Copy, Default)]
struct OptimumNode {
    cost_to_end: u32,
    item: u32,
}

/// Port of the near-optimal parser's data (`c->p.n`,
/// external/deflate/lib/deflate_compress.c:538-658 @ libdeflate v1.26).
struct NearOptimal {
    /// Binary tree matchfinder.
    bt_mf: BtMatchfinder,
    /// Cached matches for the current block: for each position, the matches found at it,
    /// sorted by strictly increasing length, then an entry whose `length` is their number and
    /// whose `offset` is the literal at the position. Its slots beyond [`MATCH_CACHE_LENGTH`]
    /// absorb the worst case overflow.
    match_cache: Vec<LzMatch>,
    /// One node per position, for the minimum-cost path algorithm, and the end-of-block one.
    optimum_nodes: Vec<OptimumNode>,
    /// The current cost model being used.
    costs: Costs,
    /// Saved cost model.
    costs_saved: Costs,
    /// The offset slot of each match offset (the full map, not the condensed one).
    offset_slot_full: Vec<u8>,
    /// Literal/match statistics saved from the previous block.
    prev_observations: [u32; NUM_OBSERVATION_TYPES],
    prev_num_observations: u32,
    /// Approximate match length frequencies based on a greedy parse, gathered during
    /// matchfinding, for setting the initial symbol costs.
    new_match_len_freqs: [u32; DEFLATE_MAX_MATCH_LEN + 1],
    match_len_freqs: [u32; DEFLATE_MAX_MATCH_LEN + 1],
    /// The maximum number of optimization passes (min-cost path searches) per block.
    max_optim_passes: u32,
    /// An optimization pass that improves the cost by fewer bits than this stops the passes.
    min_improvement_to_continue: u32,
    /// The bits a non-final pass's path must save to be regenerated and used.
    min_bits_to_use_nonfinal_path: u32,
    /// The largest block (uncompressed bytes) whose optimal static Huffman block is searched.
    max_len_to_optimize_static_block: u32,
}

impl NearOptimal {
    fn new(
        max_optim_passes: u32,
        min_improvement_to_continue: u32,
        min_bits_to_use_nonfinal_path: u32,
        max_len_to_optimize_static_block: u32,
    ) -> Box<NearOptimal> {
        let costs = Costs {
            literal: [0; DEFLATE_NUM_LITERALS],
            length: [0; DEFLATE_MAX_MATCH_LEN + 1],
            offset_slot: [0; DEFLATE_NUM_OFFSET_SYMS],
        };
        let mut n = Box::new(NearOptimal {
            bt_mf: BtMatchfinder::new(),
            match_cache: vec![
                LzMatch::default();
                MATCH_CACHE_LENGTH + MAX_MATCHES_PER_POS + DEFLATE_MAX_MATCH_LEN - 1
            ],
            optimum_nodes: vec![OptimumNode::default(); MAX_BLOCK_LENGTH + 1],
            costs: costs.clone(),
            costs_saved: costs,
            offset_slot_full: vec![0; DEFLATE_MAX_MATCH_OFFSET + 1],
            prev_observations: [0; NUM_OBSERVATION_TYPES],
            prev_num_observations: 0,
            new_match_len_freqs: [0; DEFLATE_MAX_MATCH_LEN + 1],
            match_len_freqs: [0; DEFLATE_MAX_MATCH_LEN + 1],
            max_optim_passes,
            min_improvement_to_continue,
            min_bits_to_use_nonfinal_path,
            max_len_to_optimize_static_block,
        });
        n.deflate_init_offset_slot_full();
        n
    }

    /// Port of `deflate_init_offset_slot_full` (external/deflate/lib/deflate_compress.c:3852-3868
    /// @ libdeflate v1.26).
    fn deflate_init_offset_slot_full(&mut self) {
        for offset_slot in 0..DEFLATE_OFFSET_SLOT_BASE.len() {
            let mut offset = DEFLATE_OFFSET_SLOT_BASE[offset_slot];
            let offset_end = offset + (1 << DEFLATE_EXTRA_OFFSET_BITS[offset_slot]);
            loop {
                self.offset_slot_full[offset as usize] = offset_slot as u8;
                offset += 1;
                if offset == offset_end {
                    break;
                }
            }
        }
    }

    /// Port of `deflate_set_costs_from_codes` (external/deflate/lib/deflate_compress.c:2924-2957
    /// @ libdeflate v1.26): the current cost model from the codeword lengths `lens` (the
    /// litlen lengths, then the offset ones at [`LENS_OFFSET`]).
    fn deflate_set_costs_from_codes(&mut self, lens: &[u8]) {
        // Literals
        for i in 0..DEFLATE_NUM_LITERALS {
            let bits = if lens[i] != 0 {
                u32::from(lens[i])
            } else {
                LITERAL_NOSTAT_BITS
            };
            self.costs.literal[i] = bits * BIT_COST;
        }

        // Lengths
        for i in DEFLATE_MIN_MATCH_LEN..=DEFLATE_MAX_MATCH_LEN {
            let length_slot = DEFLATE_LENGTH_SLOT[i] as usize;
            let litlen_sym = DEFLATE_FIRST_LEN_SYM + length_slot;
            let mut bits = if lens[litlen_sym] != 0 {
                u32::from(lens[litlen_sym])
            } else {
                LENGTH_NOSTAT_BITS
            };
            bits += u32::from(DEFLATE_EXTRA_LENGTH_BITS[length_slot]);
            self.costs.length[i] = bits * BIT_COST;
        }

        // Offset slots
        for i in 0..DEFLATE_OFFSET_SLOT_BASE.len() {
            let mut bits = if lens[LENS_OFFSET + i] != 0 {
                u32::from(lens[LENS_OFFSET + i])
            } else {
                OFFSET_NOSTAT_BITS
            };
            bits += u32::from(DEFLATE_EXTRA_OFFSET_BITS[i]);
            self.costs.offset_slot[i] = bits * BIT_COST;
        }
    }

    /// Port of `deflate_set_default_costs` (external/deflate/lib/deflate_compress.c:3186-3205
    /// @ libdeflate v1.26): the default symbol costs, for the first block's first
    /// optimization pass.
    fn deflate_set_default_costs(&mut self, lit_cost: u32, len_sym_cost: u32) {
        // Literals
        self.costs.literal.fill(lit_cost);

        // Lengths
        for i in DEFLATE_MIN_MATCH_LEN..=DEFLATE_MAX_MATCH_LEN {
            self.costs.length[i] = deflate_default_length_cost(i, len_sym_cost);
        }

        // Offset slots
        for i in 0..DEFLATE_OFFSET_SLOT_BASE.len() {
            self.costs.offset_slot[i] = deflate_default_offset_slot_cost(i);
        }
    }

    /// Port of `deflate_adjust_costs_impl` (external/deflate/lib/deflate_compress.c:3222-3245
    /// @ libdeflate v1.26).
    fn deflate_adjust_costs_impl(&mut self, lit_cost: u32, len_sym_cost: u32, change_amount: i32) {
        // Literals
        for c in &mut self.costs.literal {
            deflate_adjust_cost(c, lit_cost, change_amount);
        }

        // Lengths
        for i in DEFLATE_MIN_MATCH_LEN..=DEFLATE_MAX_MATCH_LEN {
            deflate_adjust_cost(
                &mut self.costs.length[i],
                deflate_default_length_cost(i, len_sym_cost),
                change_amount,
            );
        }

        // Offset slots
        for i in 0..DEFLATE_OFFSET_SLOT_BASE.len() {
            deflate_adjust_cost(
                &mut self.costs.offset_slot[i],
                deflate_default_offset_slot_cost(i),
                change_amount,
            );
        }
    }

    /// Port of `deflate_adjust_costs` (external/deflate/lib/deflate_compress.c:3256-3296
    /// @ libdeflate v1.26): mixes the current costs with the default ones when a new block
    /// begins, the more of the defaults the more the block's statistics (`split_stats`)
    /// differ from the previous block's.
    fn deflate_adjust_costs(
        &mut self,
        split_stats: &BlockSplitStats,
        lit_cost: u32,
        len_sym_cost: u32,
    ) {
        let mut total_delta: u64 = 0;

        // Decide how different the current block is from the previous block, using the block
        // splitting statistics from the current and previous blocks. The more different the
        // current block is, the more we prefer the default costs rather than the previous
        // block's costs.
        for i in 0..NUM_OBSERVATION_TYPES {
            let prev =
                u64::from(self.prev_observations[i]) * u64::from(split_stats.num_observations);
            let cur =
                u64::from(split_stats.observations[i]) * u64::from(self.prev_num_observations);
            total_delta = total_delta.wrapping_add(prev.abs_diff(cur));
        }
        let cutoff =
            (u64::from(self.prev_num_observations) * u64::from(split_stats.num_observations) * 200)
                / 512;

        if total_delta > 3 * cutoff {
            // Big change in the data; just use the default costs.
            self.deflate_set_default_costs(lit_cost, len_sym_cost);
        } else if 4 * total_delta > 9 * cutoff {
            self.deflate_adjust_costs_impl(lit_cost, len_sym_cost, 3);
        } else if 2 * total_delta > 3 * cutoff {
            self.deflate_adjust_costs_impl(lit_cost, len_sym_cost, 2);
        } else if 2 * total_delta > cutoff {
            self.deflate_adjust_costs_impl(lit_cost, len_sym_cost, 1);
        } else {
            self.deflate_adjust_costs_impl(lit_cost, len_sym_cost, 0);
        }
    }
}

/// Port of `default_litlen_costs` (external/deflate/lib/deflate_compress.c:2986-3102
/// @ libdeflate v1.26): the default cost of a literal symbol (by the number of distinct used
/// literals, 0 to 256) and of a length symbol, for an estimated match probability of 0.25, 0.5
/// and 0.75. (Copied from the source by script; the unit test reads it back from there.)
#[rustfmt::skip]
const DEFAULT_LITLEN_COSTS: [([u8; 257], u8); 3] = [
    // match_prob = 0.25
    ([
        6, 6, 22, 32, 38, 43, 48, 51,
        54, 57, 59, 61, 64, 65, 67, 69,
        70, 72, 73, 74, 75, 76, 77, 79,
        80, 80, 81, 82, 83, 84, 85, 85,
        86, 87, 88, 88, 89, 89, 90, 91,
        91, 92, 92, 93, 93, 94, 95, 95,
        96, 96, 96, 97, 97, 98, 98, 99,
        99, 99, 100, 100, 101, 101, 101, 102,
        102, 102, 103, 103, 104, 104, 104, 105,
        105, 105, 105, 106, 106, 106, 107, 107,
        107, 108, 108, 108, 108, 109, 109, 109,
        109, 110, 110, 110, 111, 111, 111, 111,
        112, 112, 112, 112, 112, 113, 113, 113,
        113, 114, 114, 114, 114, 114, 115, 115,
        115, 115, 115, 116, 116, 116, 116, 116,
        117, 117, 117, 117, 117, 118, 118, 118,
        118, 118, 118, 119, 119, 119, 119, 119,
        120, 120, 120, 120, 120, 120, 121, 121,
        121, 121, 121, 121, 121, 122, 122, 122,
        122, 122, 122, 123, 123, 123, 123, 123,
        123, 123, 124, 124, 124, 124, 124, 124,
        124, 125, 125, 125, 125, 125, 125, 125,
        125, 126, 126, 126, 126, 126, 126, 126,
        127, 127, 127, 127, 127, 127, 127, 127,
        128, 128, 128, 128, 128, 128, 128, 128,
        128, 129, 129, 129, 129, 129, 129, 129,
        129, 129, 130, 130, 130, 130, 130, 130,
        130, 130, 130, 131, 131, 131, 131, 131,
        131, 131, 131, 131, 131, 132, 132, 132,
        132, 132, 132, 132, 132, 132, 132, 133,
        133, 133, 133, 133, 133, 133, 133, 133,
        133, 134, 134, 134, 134, 134, 134, 134,
        134,
    ], 109),
    // match_prob = 0.5
    ([
        16, 16, 32, 41, 48, 53, 57, 60,
        64, 66, 69, 71, 73, 75, 76, 78,
        80, 81, 82, 83, 85, 86, 87, 88,
        89, 90, 91, 92, 92, 93, 94, 95,
        96, 96, 97, 98, 98, 99, 99, 100,
        101, 101, 102, 102, 103, 103, 104, 104,
        105, 105, 106, 106, 107, 107, 108, 108,
        108, 109, 109, 110, 110, 110, 111, 111,
        112, 112, 112, 113, 113, 113, 114, 114,
        114, 115, 115, 115, 115, 116, 116, 116,
        117, 117, 117, 118, 118, 118, 118, 119,
        119, 119, 119, 120, 120, 120, 120, 121,
        121, 121, 121, 122, 122, 122, 122, 122,
        123, 123, 123, 123, 124, 124, 124, 124,
        124, 125, 125, 125, 125, 125, 126, 126,
        126, 126, 126, 127, 127, 127, 127, 127,
        128, 128, 128, 128, 128, 128, 129, 129,
        129, 129, 129, 129, 130, 130, 130, 130,
        130, 130, 131, 131, 131, 131, 131, 131,
        131, 132, 132, 132, 132, 132, 132, 133,
        133, 133, 133, 133, 133, 133, 134, 134,
        134, 134, 134, 134, 134, 134, 135, 135,
        135, 135, 135, 135, 135, 135, 136, 136,
        136, 136, 136, 136, 136, 136, 137, 137,
        137, 137, 137, 137, 137, 137, 138, 138,
        138, 138, 138, 138, 138, 138, 138, 139,
        139, 139, 139, 139, 139, 139, 139, 139,
        140, 140, 140, 140, 140, 140, 140, 140,
        140, 141, 141, 141, 141, 141, 141, 141,
        141, 141, 141, 142, 142, 142, 142, 142,
        142, 142, 142, 142, 142, 142, 143, 143,
        143, 143, 143, 143, 143, 143, 143, 143,
        144,
    ], 93),
    // match_prob = 0.75
    ([
        32, 32, 48, 57, 64, 69, 73, 76,
        80, 82, 85, 87, 89, 91, 92, 94,
        96, 97, 98, 99, 101, 102, 103, 104,
        105, 106, 107, 108, 108, 109, 110, 111,
        112, 112, 113, 114, 114, 115, 115, 116,
        117, 117, 118, 118, 119, 119, 120, 120,
        121, 121, 122, 122, 123, 123, 124, 124,
        124, 125, 125, 126, 126, 126, 127, 127,
        128, 128, 128, 129, 129, 129, 130, 130,
        130, 131, 131, 131, 131, 132, 132, 132,
        133, 133, 133, 134, 134, 134, 134, 135,
        135, 135, 135, 136, 136, 136, 136, 137,
        137, 137, 137, 138, 138, 138, 138, 138,
        139, 139, 139, 139, 140, 140, 140, 140,
        140, 141, 141, 141, 141, 141, 142, 142,
        142, 142, 142, 143, 143, 143, 143, 143,
        144, 144, 144, 144, 144, 144, 145, 145,
        145, 145, 145, 145, 146, 146, 146, 146,
        146, 146, 147, 147, 147, 147, 147, 147,
        147, 148, 148, 148, 148, 148, 148, 149,
        149, 149, 149, 149, 149, 149, 150, 150,
        150, 150, 150, 150, 150, 150, 151, 151,
        151, 151, 151, 151, 151, 151, 152, 152,
        152, 152, 152, 152, 152, 152, 153, 153,
        153, 153, 153, 153, 153, 153, 154, 154,
        154, 154, 154, 154, 154, 154, 154, 155,
        155, 155, 155, 155, 155, 155, 155, 155,
        156, 156, 156, 156, 156, 156, 156, 156,
        156, 157, 157, 157, 157, 157, 157, 157,
        157, 157, 157, 158, 158, 158, 158, 158,
        158, 158, 158, 158, 158, 158, 159, 159,
        159, 159, 159, 159, 159, 159, 159, 159,
        160,
    ], 84),
];
/// Port of `deflate_default_length_cost` (external/deflate/lib/deflate_compress.c:3162-3169
/// @ libdeflate v1.26).
#[inline]
fn deflate_default_length_cost(len: usize, len_sym_cost: u32) -> u32 {
    let slot = DEFLATE_LENGTH_SLOT[len] as usize;
    let num_extra_bits = u32::from(DEFLATE_EXTRA_LENGTH_BITS[slot]);
    len_sym_cost + (num_extra_bits * BIT_COST)
}

/// Port of `deflate_default_offset_slot_cost` (external/deflate/lib/deflate_compress.c:3171-3183
/// @ libdeflate v1.26).
#[inline]
fn deflate_default_offset_slot_cost(slot: usize) -> u32 {
    let num_extra_bits = u32::from(DEFLATE_EXTRA_OFFSET_BITS[slot]);
    // Assume that all offset symbols are equally probable. The resulting cost is
    // 'int(-log2(1/30) * BIT_COST)', where 30 is the number of potentially-used offset
    // symbols.
    let offset_sym_cost = 4 * BIT_COST + (907 * BIT_COST) / 1000;
    offset_sym_cost + (num_extra_bits * BIT_COST)
}

/// Port of `deflate_adjust_cost` (external/deflate/lib/deflate_compress.c:3207-3220
/// @ libdeflate v1.26).
#[inline]
fn deflate_adjust_cost(cost_p: &mut u32, default_cost: u32, change_amount: i32) {
    let c = *cost_p;
    *cost_p = if change_amount == 0 {
        // Block is very similar to previous; prefer previous costs.
        default_cost.wrapping_add(c.wrapping_mul(3)) / 4
    } else if change_amount == 1 {
        default_cost.wrapping_add(c) / 2
    } else if change_amount == 2 {
        default_cost.wrapping_mul(5).wrapping_add(c.wrapping_mul(3)) / 8
    } else {
        // Block differs greatly from previous; prefer default costs.
        default_cost.wrapping_mul(3).wrapping_add(c) / 4
    };
}

impl Compressor {
    /// Port of `deflate_tally_item_list` (external/deflate/lib/deflate_compress.c:2843-2868
    /// @ libdeflate v1.26): the symbol frequencies of the minimum-cost path's literals and
    /// matches, and of the end-of-block symbol.
    fn deflate_tally_item_list(&mut self, n: &NearOptimal, block_length: u32) {
        let end_node = block_length as usize;
        let mut cur_node = 0usize;
        loop {
            let item = n.optimum_nodes[cur_node].item;
            let length = item & OPTIMUM_LEN_MASK;
            let offset = item >> OPTIMUM_OFFSET_SHIFT;

            if length == 1 {
                // Literal
                self.freqs.litlen[offset as usize] += 1;
            } else {
                // Match
                self.freqs.litlen
                    [DEFLATE_FIRST_LEN_SYM + DEFLATE_LENGTH_SLOT[length as usize] as usize] += 1;
                self.freqs.offset[n.offset_slot_full[offset as usize] as usize] += 1;
            }
            cur_node += length as usize;
            if cur_node == end_node {
                break;
            }
        }

        // Tally the end-of-block symbol.
        self.freqs.litlen[DEFLATE_END_OF_BLOCK] += 1;
    }

    /// Port of `deflate_choose_all_literals` (external/deflate/lib/deflate_compress.c:2870-2882
    /// @ libdeflate v1.26): the frequencies and codes of the block as literals only.
    fn deflate_choose_all_literals(&mut self, block: &[u8]) {
        self.deflate_reset_symbol_frequencies();
        for &b in block {
            self.freqs.litlen[b as usize] += 1;
        }
        self.freqs.litlen[DEFLATE_END_OF_BLOCK] += 1;

        deflate_make_huffman_codes(&self.freqs, &mut self.codes);
    }

    /// Port of `deflate_compute_true_cost` (external/deflate/lib/deflate_compress.c:2889-2921
    /// @ libdeflate v1.26): the exact cost, in bits, of the matches and literals of the
    /// frequencies as a dynamic Huffman block, with the codes already built.
    fn deflate_compute_true_cost(&mut self) -> u32 {
        let mut cost: u32 = 0;

        self.deflate_precompute_huffman_header();

        let p = &*self.precode;
        self.codes.lens[p.num_litlen_syms..DEFLATE_NUM_LITLEN_SYMS].fill(0);

        cost = cost.wrapping_add(5 + 5 + 4 + (3 * p.num_explicit_lens as u32));
        for sym in 0..DEFLATE_NUM_PRECODE_SYMS {
            cost =
                cost.wrapping_add(p.freqs[sym].wrapping_mul(
                    u32::from(p.lens[sym]) + u32::from(DEFLATE_EXTRA_PRECODE_BITS[sym]),
                ));
        }

        let lens = &self.codes.lens;
        let f = &self.freqs;
        for sym in 0..DEFLATE_FIRST_LEN_SYM {
            cost = cost.wrapping_add(f.litlen[sym].wrapping_mul(u32::from(lens[sym])));
        }

        for sym in DEFLATE_FIRST_LEN_SYM..DEFLATE_FIRST_LEN_SYM + DEFLATE_EXTRA_LENGTH_BITS.len() {
            cost = cost.wrapping_add(f.litlen[sym].wrapping_mul(
                u32::from(lens[sym])
                    + u32::from(DEFLATE_EXTRA_LENGTH_BITS[sym - DEFLATE_FIRST_LEN_SYM]),
            ));
        }

        for sym in 0..DEFLATE_EXTRA_OFFSET_BITS.len() {
            cost = cost.wrapping_add(f.offset[sym].wrapping_mul(
                u32::from(lens[LENS_OFFSET + sym]) + u32::from(DEFLATE_EXTRA_OFFSET_BITS[sym]),
            ));
        }
        cost
    }

    /// Port of `deflate_choose_default_litlen_costs`
    /// (external/deflate/lib/deflate_compress.c:3108-3160 @ libdeflate v1.26): the default costs of
    /// a literal and of a length symbol, from the number of distinct literals of the block and the
    /// match frequency of a greedy parse.
    fn deflate_choose_default_litlen_costs(&mut self, n: &NearOptimal, block: &[u8]) -> (u32, u32) {
        let mut num_used_literals: u32 = 0;
        let mut literal_freq = block.len() as u32;

        // Calculate the number of distinct literals that exist in the data.
        self.freqs.litlen[..DEFLATE_NUM_LITERALS].fill(0);
        let cutoff = literal_freq >> 11; // Ignore literals used very rarely.
        for &b in block {
            self.freqs.litlen[b as usize] += 1;
        }
        for i in 0..DEFLATE_NUM_LITERALS {
            if self.freqs.litlen[i] > cutoff {
                num_used_literals += 1;
            }
        }
        if num_used_literals == 0 {
            num_used_literals = 1;
        }

        // Estimate the relative frequency of literals and matches in the optimal parsing
        // solution. We don't know the optimal solution, so this can only be a very rough
        // estimate. Therefore, we basically use the match frequency from a greedy parse. We
        // also apply the min_len heuristic used by the greedy and lazy parsers, to avoid
        // counting too many matches when literals are cheaper than short matches.
        let mut match_freq: u32 = 0;
        let mut i = choose_min_match_len(num_used_literals, self.max_search_depth);
        while (i as usize) < n.match_len_freqs.len() {
            match_freq = match_freq.wrapping_add(n.match_len_freqs[i as usize]);
            literal_freq = literal_freq.wrapping_sub(i.wrapping_mul(n.match_len_freqs[i as usize]));
            i += 1;
        }
        if (literal_freq as i32) < 0 {
            // shouldn't happen
            literal_freq = 0;
        }

        let i = if match_freq > literal_freq {
            2 // many matches
        } else if match_freq.wrapping_mul(4) > literal_freq {
            1 // neutral
        } else {
            0 // few matches
        };

        // BIT_COST == 16
        let (used_lits_to_lit_cost, len_sym_cost) = &DEFAULT_LITLEN_COSTS[i];
        (
            u32::from(used_lits_to_lit_cost[num_used_literals as usize]),
            u32::from(*len_sym_cost),
        )
    }

    /// Port of `deflate_set_initial_costs` (external/deflate/lib/deflate_compress.c:3298-3311
    /// @ libdeflate v1.26).
    fn deflate_set_initial_costs(
        &mut self,
        n: &mut NearOptimal,
        block: &[u8],
        is_first_block: bool,
    ) {
        let (lit_cost, len_sym_cost) = self.deflate_choose_default_litlen_costs(n, block);
        if is_first_block {
            n.deflate_set_default_costs(lit_cost, len_sym_cost);
        } else {
            n.deflate_adjust_costs(&self.split_stats, lit_cost, len_sym_cost);
        }
    }

    /// Port of `deflate_find_min_cost_path` (external/deflate/lib/deflate_compress.c:3327-3399
    /// @ libdeflate v1.26): the minimum-cost path through the block's graph of literal and
    /// match choices under the cost model, from the end node back to the first (`cache_ptr`
    /// is the end of the block's cached matches); then the frequencies and codes of the path.
    fn deflate_find_min_cost_path(
        &mut self,
        n: &mut NearOptimal,
        block_length: u32,
        mut cache_ptr: usize,
    ) {
        let end_node = block_length as usize;
        let mut cur_node = end_node;

        n.optimum_nodes[cur_node].cost_to_end = 0;
        loop {
            cur_node -= 1;
            cache_ptr -= 1;

            let num_matches = usize::from(n.match_cache[cache_ptr].length);
            let literal = u32::from(n.match_cache[cache_ptr].offset);

            // It's always possible to choose a literal.
            let mut best_cost_to_end = n.costs.literal[literal as usize]
                .wrapping_add(n.optimum_nodes[cur_node + 1].cost_to_end);
            n.optimum_nodes[cur_node].item = (literal << OPTIMUM_OFFSET_SHIFT) | 1;

            // Also consider matches if there are any.
            if num_matches != 0 {
                // Consider each length from the minimum (DEFLATE_MIN_MATCH_LEN) to the length
                // of the longest match found at this position. For each length, we consider
                // only the smallest offset for which that length is available. Although this
                // is not guaranteed to be optimal due to the possibility of a larger offset
                // costing less than a smaller offset to code, this is a very useful heuristic.
                let mut m = cache_ptr - num_matches;
                let mut len = DEFLATE_MIN_MATCH_LEN as u32;
                loop {
                    let offset = u32::from(n.match_cache[m].offset);
                    let offset_slot = n.offset_slot_full[offset as usize] as usize;
                    let offset_cost = n.costs.offset_slot[offset_slot];
                    let match_length = u32::from(n.match_cache[m].length);
                    loop {
                        let cost_to_end = offset_cost
                            .wrapping_add(n.costs.length[len as usize])
                            .wrapping_add(n.optimum_nodes[cur_node + len as usize].cost_to_end);
                        if cost_to_end < best_cost_to_end {
                            best_cost_to_end = cost_to_end;
                            n.optimum_nodes[cur_node].item = len | (offset << OPTIMUM_OFFSET_SHIFT);
                        }
                        len += 1;
                        if len > match_length {
                            break;
                        }
                    }
                    m += 1;
                    if m == cache_ptr {
                        break;
                    }
                }
                cache_ptr -= num_matches;
            }
            n.optimum_nodes[cur_node].cost_to_end = best_cost_to_end;
            if cur_node == 0 {
                break;
            }
        }

        self.deflate_reset_symbol_frequencies();
        self.deflate_tally_item_list(n, block_length);
        deflate_make_huffman_codes(&self.freqs, &mut self.codes);
    }

    /// Port of `deflate_optimize_and_flush_block`
    /// (external/deflate/lib/deflate_compress.c:3416-3530 @ libdeflate v1.26): chooses the block's
    /// literals and matches (the minimum-cost path over several passes, each pass's costs from the
    /// previous pass's codes; or only literals; or the optimal static block's path), then outputs
    /// the block. `used_only_literals` says whether only literals were best.
    #[allow(clippy::too_many_arguments)]
    fn deflate_optimize_and_flush_block(
        &mut self,
        n: &mut NearOptimal,
        os: &mut OutputBitstream<'_>,
        data: &[u8],
        block_begin: usize,
        block_length: u32,
        cache_ptr: usize,
        is_first_block: bool,
        is_final_block: bool,
        used_only_literals: &mut bool,
    ) {
        let block = &data[block_begin..block_begin + block_length as usize];
        let mut num_passes_remaining = n.max_optim_passes;
        let mut best_true_cost = u32::MAX;
        let mut true_cost;
        let mut static_cost = u32::MAX;
        let mut seq: Option<Sequence> = None;

        // On some data, using only literals (no matches) ends up being better than what the
        // iterative optimization algorithm produces. Therefore, consider using only literals.
        self.deflate_choose_all_literals(block);
        let only_lits_cost = self.deflate_compute_true_cost();

        // Force the block to really end at the desired length, even if some matches extend
        // beyond it.
        let last =
            ((block_length - 1) as usize + DEFLATE_MAX_MATCH_LEN).min(n.optimum_nodes.len() - 1);
        for node in &mut n.optimum_nodes[block_length as usize..=last] {
            node.cost_to_end = 0x80000000;
        }

        // Sometimes a static Huffman block ends up being cheapest, particularly if the block
        // is small. So, if the block is sufficiently small, find the optimal static block
        // solution and remember its cost.
        if block_length <= n.max_len_to_optimize_static_block {
            // Save c->p.n.costs temporarily.
            n.costs_saved = n.costs.clone();

            n.deflate_set_costs_from_codes(&self.static_codes.lens);
            self.deflate_find_min_cost_path(n, block_length, cache_ptr);
            static_cost = n.optimum_nodes[0].cost_to_end / BIT_COST;
            static_cost = static_cost.wrapping_add(7); // for the end-of-block symbol

            // Restore c->p.n.costs.
            n.costs = n.costs_saved.clone();
        }

        // Initialize c->p.n.costs with default costs.
        self.deflate_set_initial_costs(n, block, is_first_block);

        loop {
            // Find the minimum-cost path for this pass. Also set c->freqs and c->codes to
            // match the path.
            self.deflate_find_min_cost_path(n, block_length, cache_ptr);

            // Compute the exact cost of the block if the path were to be used. Note that this
            // differs from c->p.n.optimum_nodes[0].cost_to_end in that true_cost uses the
            // actual Huffman codes instead of c->p.n.costs.
            true_cost = self.deflate_compute_true_cost();

            // If the cost didn't improve much from the previous pass, then doing more passes
            // probably won't be helpful, so stop early.
            if true_cost.wrapping_add(n.min_improvement_to_continue) > best_true_cost {
                break;
            }

            best_true_cost = true_cost;

            // Save the cost model that gave 'best_true_cost'.
            n.costs_saved = n.costs.clone();

            // Update the cost model from the Huffman codes.
            n.deflate_set_costs_from_codes(&self.codes.lens);

            num_passes_remaining -= 1;
            if num_passes_remaining == 0 {
                break;
            }
        }

        *used_only_literals = false;
        if only_lits_cost.min(static_cost) < best_true_cost {
            if only_lits_cost < static_cost {
                // Using only literals ended up being best!
                self.deflate_choose_all_literals(block);
                n.deflate_set_costs_from_codes(&self.codes.lens);
                seq = Some(Sequence {
                    litrunlen_and_length: block_length,
                    ..Sequence::default()
                });
                *used_only_literals = true;
            } else {
                // Static block ended up being best!
                n.deflate_set_costs_from_codes(&self.static_codes.lens);
                self.deflate_find_min_cost_path(n, block_length, cache_ptr);
            }
        } else if true_cost >= best_true_cost.wrapping_add(n.min_bits_to_use_nonfinal_path) {
            // The best solution was actually from a non-final optimization pass, so recover
            // and use the min-cost path from that pass.
            n.costs = n.costs_saved.clone();
            self.deflate_find_min_cost_path(n, block_length, cache_ptr);
            n.deflate_set_costs_from_codes(&self.codes.lens);
        }
        let items = match &seq {
            Some(s) => Items::Sequences(std::slice::from_ref(s)),
            None => Items::Optimum(&*n),
        };
        self.deflate_flush_block(os, data, block_begin, block_length, items, is_final_block);
    }

    /// Port of `deflate_near_optimal_init_stats`
    /// (external/deflate/lib/deflate_compress.c:3532-3539 @ libdeflate v1.26).
    fn deflate_near_optimal_init_stats(&mut self, n: &mut NearOptimal) {
        self.split_stats.init();
        n.new_match_len_freqs.fill(0);
        n.match_len_freqs.fill(0);
    }

    /// Port of `deflate_near_optimal_merge_stats`
    /// (external/deflate/lib/deflate_compress.c:3541-3551 @ libdeflate v1.26).
    fn deflate_near_optimal_merge_stats(&mut self, n: &mut NearOptimal) {
        self.split_stats.merge_new_observations();
        for i in 0..n.match_len_freqs.len() {
            n.match_len_freqs[i] = n.match_len_freqs[i].wrapping_add(n.new_match_len_freqs[i]);
            n.new_match_len_freqs[i] = 0;
        }
    }

    /// Port of `deflate_near_optimal_save_stats` (external/deflate/lib/deflate_compress.c:3558-3566
    /// @ libdeflate v1.26): the previous block's literal/match statistics, for
    /// `deflate_adjust_costs`.
    fn deflate_near_optimal_save_stats(&self, n: &mut NearOptimal) {
        n.prev_observations = self.split_stats.observations;
        n.prev_num_observations = self.split_stats.num_observations;
    }

    /// Port of `deflate_near_optimal_clear_old_stats`
    /// (external/deflate/lib/deflate_compress.c:3568-3577 @ libdeflate v1.26).
    fn deflate_near_optimal_clear_old_stats(&mut self, n: &mut NearOptimal) {
        self.split_stats.observations = [0; NUM_OBSERVATION_TYPES];
        self.split_stats.num_observations = 0;
        n.match_len_freqs.fill(0);
    }

    /// Port of `deflate_compress_near_optimal`
    /// (external/deflate/lib/deflate_compress.c:3592-3849 @ libdeflate v1.26): the "near-optimal"
    /// DEFLATE compressor (levels 10 to 12), the optimal representation of each block by a
    /// minimum-cost path search over the graph of its literal and match choices, under an
    /// iterated cost model.
    fn deflate_compress_near_optimal(&mut self, data: &[u8], os: &mut OutputBitstream<'_>) {
        let in_end = data.len();
        let mut in_next = 0usize;
        let mut in_block_begin = in_next;
        let mut in_cur_base = in_next;
        let mut in_next_slide = in_next + (in_end - in_next).min(WINDOW_SIZE);
        let mut max_len = DEFLATE_MAX_MATCH_LEN as u32;
        let mut nice_len = self.nice_match_length.min(max_len);
        let mut n = self.n.take().expect("the near-optimal parser's data");
        let mut cache_ptr = 0usize;
        let mut next_hashes = [0u32; 2];
        let mut prev_block_used_only_literals = false;
        let depth = self.max_search_depth;

        n.bt_mf.init();
        self.deflate_near_optimal_init_stats(&mut n);

        loop {
            // Starting a new DEFLATE block
            let in_max_block_end =
                choose_max_block_end(in_block_begin, in_end, SOFT_MAX_BLOCK_LENGTH);
            let mut prev_end_block_check: Option<usize> = None;
            let mut change_detected = false;
            let mut next_observation = in_next;

            // Use the minimum match length heuristic to improve the literal/match statistics
            // gathered during matchfinding. However, the actual near-optimal parse won't
            // respect min_len, as it can accurately assess the costs of different matches.
            //
            // If the "use only literals" strategy happened to be the best strategy on the
            // previous block, then probably the min_match_len heuristic is still not
            // aggressive enough for the data, so force gathering literal stats only.
            let min_len = if prev_block_used_only_literals {
                DEFLATE_MAX_MATCH_LEN as u32 + 1
            } else {
                calculate_min_match_len(&data[in_block_begin..in_max_block_end], depth)
            };

            // Find matches until we decide to end the block. We end the block if any of the
            // following is true:
            //
            // (1) Maximum block length has been reached
            // (2) Match catch may overflow.
            // (3) Block split heuristic says to split now.
            loop {
                let mut remaining = in_end - in_next;

                // Slide the window forward if needed.
                if in_next == in_next_slide {
                    n.bt_mf.slide_window();
                    in_cur_base = in_next;
                    in_next_slide = in_next + remaining.min(WINDOW_SIZE);
                }

                // Find matches with the current position using the binary tree matchfinder
                // and save them in match_cache.
                let matches = cache_ptr;
                let mut best_len: u32 = 0;
                adjust_max_and_nice_len(&mut max_len, &mut nice_len, remaining);
                if max_len >= BT_MATCHFINDER_REQUIRED_NBYTES {
                    let NearOptimal {
                        bt_mf, match_cache, ..
                    } = &mut *n;
                    cache_ptr = bt_mf.get_matches(
                        data,
                        in_cur_base,
                        (in_next - in_cur_base) as i32,
                        max_len,
                        nice_len,
                        depth,
                        &mut next_hashes,
                        match_cache,
                        matches,
                    );
                    if cache_ptr > matches {
                        best_len = u32::from(n.match_cache[cache_ptr - 1].length);
                    }
                }
                if in_next >= next_observation {
                    if best_len >= min_len {
                        self.split_stats.observe_match(best_len);
                        next_observation = in_next + best_len as usize;
                        n.new_match_len_freqs[best_len as usize] += 1;
                    } else {
                        self.split_stats.observe_literal(data[in_next]);
                        next_observation = in_next + 1;
                    }
                }

                n.match_cache[cache_ptr] = LzMatch {
                    length: (cache_ptr - matches) as u16,
                    offset: u16::from(data[in_next]),
                };
                in_next += 1;
                cache_ptr += 1;

                // If there was a very long match found, don't cache any matches for the bytes
                // covered by that match. This avoids degenerate behavior when compressing
                // highly redundant data, where the number of matches can be very large.
                //
                // This heuristic doesn't actually hurt the compression ratio very much. If
                // there's a long match, then the data must be highly compressible, so it
                // doesn't matter much what we do.
                if best_len >= DEFLATE_MIN_MATCH_LEN as u32 && best_len >= nice_len {
                    best_len -= 1;
                    loop {
                        remaining = in_end - in_next;
                        if in_next == in_next_slide {
                            n.bt_mf.slide_window();
                            in_cur_base = in_next;
                            in_next_slide = in_next + remaining.min(WINDOW_SIZE);
                        }
                        adjust_max_and_nice_len(&mut max_len, &mut nice_len, remaining);
                        if max_len >= BT_MATCHFINDER_REQUIRED_NBYTES {
                            n.bt_mf.skip_byte(
                                data,
                                in_cur_base,
                                (in_next - in_cur_base) as i32,
                                nice_len,
                                depth,
                                &mut next_hashes,
                            );
                        }
                        n.match_cache[cache_ptr] = LzMatch {
                            length: 0,
                            offset: u16::from(data[in_next]),
                        };
                        in_next += 1;
                        cache_ptr += 1;
                        best_len -= 1;
                        if best_len == 0 {
                            break;
                        }
                    }
                }
                // Maximum block length or end of input reached?
                if in_next >= in_max_block_end {
                    break;
                }
                // Match cache overflowed?
                if cache_ptr >= MATCH_CACHE_LENGTH {
                    break;
                }
                // Not ready to try to end the block (again)?
                if !self
                    .split_stats
                    .ready_to_check_block(in_block_begin, in_next, in_end)
                {
                    continue;
                }
                // Check if it would be worthwhile to end the block.
                if self
                    .split_stats
                    .do_end_block_check((in_next - in_block_begin) as u32)
                {
                    change_detected = true;
                    break;
                }
                // Ending the block doesn't seem worthwhile here.
                self.deflate_near_optimal_merge_stats(&mut n);
                prev_end_block_check = Some(in_next);
            }
            // All the matches for this block have been cached. Now choose the precise end of
            // the block and the sequence of items to output to represent it, then flush the
            // block.
            if let (true, Some(in_block_end)) = (change_detected, prev_end_block_check) {
                // The block is being ended because a recent chunk of data differs from the
                // rest of the block. We could end the block at 'in_next' like the greedy and
                // lazy compressors do, but that's not ideal since it would include the
                // differing chunk in the block. The near-optimal compressor has time to do a
                // better job. Therefore, we rewind to just before the chunk, and output a
                // block that only goes up to there.
                //
                // We then set things up to correctly start the next block, considering that
                // some work has already been done on it (some matches found and stats
                // gathered).
                let orig_cache_ptr = cache_ptr;
                let block_length = (in_block_end - in_block_begin) as u32;
                let is_first = in_block_begin == 0;
                let is_final = false;
                let mut num_bytes_to_rewind = in_next - in_block_end;

                // Rewind the match cache.
                loop {
                    cache_ptr -= 1;
                    cache_ptr -= usize::from(n.match_cache[cache_ptr].length);
                    num_bytes_to_rewind -= 1;
                    if num_bytes_to_rewind == 0 {
                        break;
                    }
                }
                let cache_len_rewound = orig_cache_ptr - cache_ptr;

                self.deflate_optimize_and_flush_block(
                    &mut n,
                    os,
                    data,
                    in_block_begin,
                    block_length,
                    cache_ptr,
                    is_first,
                    is_final,
                    &mut prev_block_used_only_literals,
                );
                n.match_cache.copy_within(cache_ptr..orig_cache_ptr, 0);
                cache_ptr = cache_len_rewound;
                self.deflate_near_optimal_save_stats(&mut n);
                // Clear the stats for the just-flushed block, leaving just the stats for the
                // beginning of the next block.
                self.deflate_near_optimal_clear_old_stats(&mut n);
                in_block_begin = in_block_end;
            } else {
                // The block is being ended for a reason other than a differing data chunk
                // being detected. Don't rewind at all; just end the block at the current
                // position.
                let block_length = (in_next - in_block_begin) as u32;
                let is_first = in_block_begin == 0;
                let is_final = in_next == in_end;

                self.deflate_near_optimal_merge_stats(&mut n);
                self.deflate_optimize_and_flush_block(
                    &mut n,
                    os,
                    data,
                    in_block_begin,
                    block_length,
                    cache_ptr,
                    is_first,
                    is_final,
                    &mut prev_block_used_only_literals,
                );
                cache_ptr = 0;
                self.deflate_near_optimal_save_stats(&mut n);
                self.deflate_near_optimal_init_stats(&mut n);
                in_block_begin = in_next;
            }
            if in_next == in_end || os.overflow {
                break;
            }
        }
        self.n = Some(n);
    }
}

/// `WRITE_MATCH` (external/deflate/lib/deflate_compress.c:1661-1694 @ libdeflate v1.26): the
/// litlen symbol and extra length bits, the offset symbol, and the extra offset bits of a
/// match, with the flushes a 64-bit bit buffer needs.
#[inline]
fn write_match(
    os: &mut OutputBitstream<'_>,
    out_fast_end: usize,
    length: &LengthCodes,
    codes: &Codes,
    length_: u32,
    offset: u32,
    offset_slot: u32,
) {
    let l = length_ as usize;
    let s = offset_slot as usize;
    // Litlen symbol and extra length bits
    os.add_bits(length.codewords[l], u32::from(length.lens[l]));

    if !can_buffer(
        MAX_LITLEN_CODEWORD_LEN
            + DEFLATE_MAX_EXTRA_LENGTH_BITS
            + MAX_OFFSET_CODEWORD_LEN
            + DEFLATE_MAX_EXTRA_OFFSET_BITS,
    ) {
        os.flush_bits(out_fast_end);
    }

    // Offset symbol
    os.add_bits(
        codes.codewords_offset[s],
        u32::from(codes.lens[LENS_OFFSET + s]),
    );

    if !can_buffer(MAX_OFFSET_CODEWORD_LEN + DEFLATE_MAX_EXTRA_OFFSET_BITS) {
        os.flush_bits(out_fast_end);
    }

    // Extra offset bits
    os.add_bits(
        offset - DEFLATE_OFFSET_SLOT_BASE[s],
        u32::from(DEFLATE_EXTRA_OFFSET_BITS[s]),
    );

    os.flush_bits(out_fast_end);
}

/// Port of `libdeflate_deflate_compress_bound` (external/deflate/lib/deflate_compress.c:4087-4135
/// @ libdeflate v1.26): the most bytes a DEFLATE stream of `in_nbytes` bytes takes, at any
/// compression level (upstream's compressor argument is unused).
pub fn deflate_compress_bound(in_nbytes: usize) -> usize {
    // Since the compressor never uses a compressed block when an uncompressed block is
    // cheaper, the worst case can be no worse than the case where only uncompressed blocks
    // are used. Each uncompressed block has 5 bytes of overhead, for the BFINAL, BTYPE, LEN,
    // and NLEN fields; empty inputs need 1 (empty) block.
    let max_blocks = in_nbytes.div_ceil(MIN_BLOCK_LENGTH).max(1);
    5usize.wrapping_mul(max_blocks).wrapping_add(in_nbytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `min_lens` as upstream writes it (external/deflate/lib/deflate_compress.c:2299-2306
    /// @ libdeflate v1.26), read from the source.
    fn upstream_min_lens() -> Vec<u32> {
        let src = include_str!("../../../upstream/openexr/external/deflate/lib/deflate_compress.c");
        let start = src
            .find("static const u8 min_lens[] = {")
            .expect("min_lens")
            + "static const u8 min_lens[] = {".len();
        let body = &src[start..start + src[start..].find("};").expect("its end")];
        let mut values = Vec::new();
        for line in body.lines() {
            let line = line.split("/*").next().unwrap_or("");
            for v in line.split(',') {
                let v = v.trim();
                if !v.is_empty() {
                    values.push(v.parse().expect("a number"));
                }
            }
        }
        values
    }

    /// `default_litlen_costs` as upstream writes it
    /// (external/deflate/lib/deflate_compress.c:2986-3102 @ libdeflate v1.26), read from the
    /// source: each table's 257 literal costs and its length symbol cost.
    fn upstream_default_litlen_costs() -> Vec<(Vec<u8>, u8)> {
        let src = include_str!("../../../upstream/openexr/external/deflate/lib/deflate_compress.c");
        let start = src
            .find("} default_litlen_costs[] = {")
            .expect("default_litlen_costs");
        let body = &src[start..start + src[start..].find("\n};").expect("its end")];
        let mut tables = Vec::new();
        let mut rest = body;
        while let Some(at) = rest.find(".used_lits_to_lit_cost = {") {
            rest = &rest[at + ".used_lits_to_lit_cost = {".len()..];
            let end = rest.find('}').expect("the table's end");
            let costs: Vec<u8> = rest[..end]
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.parse().expect("a number"))
                .collect();
            rest = &rest[end..];
            let at =
                rest.find(".len_sym_cost = ").expect("len_sym_cost") + ".len_sym_cost = ".len();
            rest = &rest[at..];
            let end = rest.find(',').expect("its end");
            tables.push((costs, rest[..end].trim().parse().expect("a number")));
        }
        tables
    }

    /// The default literal and length symbol costs are upstream's table.
    #[test]
    fn default_litlen_costs_are_upstreams() {
        let want = upstream_default_litlen_costs();
        assert_eq!(want.len(), DEFAULT_LITLEN_COSTS.len());
        for (i, (costs, len_sym_cost)) in want.iter().enumerate() {
            assert_eq!(&costs[..], &DEFAULT_LITLEN_COSTS[i].0[..], "table {i}");
            assert_eq!(*len_sym_cost, DEFAULT_LITLEN_COSTS[i].1, "table {i}");
        }
    }

    /// `choose_min_match_len` gives upstream's table entry for every literal count (and 3
    /// past it), at a search depth that leaves it unchanged.
    #[test]
    fn min_match_len_table_is_upstreams() {
        let table = upstream_min_lens();
        assert_eq!(table.len(), 80);
        for n in 0..=256u32 {
            let want = table.get(n as usize).copied().unwrap_or(3);
            assert_eq!(choose_min_match_len(n, 16), want, "{n} literals");
        }
    }
}
