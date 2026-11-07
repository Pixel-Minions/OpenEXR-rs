// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `deflate_decompress.c` and `decompress_template.h`
//! (external/deflate/lib/deflate_decompress.c:1-1208 @ libdeflate v1.26, and
//! external/deflate/lib/decompress_template.h:1-777 @ libdeflate
//! v1.26): the DEFLATE decompressor, for x86-64 (a 64-bit bit buffer, unaligned word access).
//!
//! The decode loop is the template's: a "fastloop" while the input and output are far from
//! their ends, which refills the bit buffer a word at a time and copies matches a word at a
//! time (writing a little past each match, inside the output, as upstream does), then the
//! generic loop. The decoder's tables, which upstream overlays in a union, are separate arrays
//! here; upstream's code never reads one through another, so nothing changes.

use crate::LibdeflateResult;
use crate::deflate_constants::*;

// The 64-bit bit buffer of x86-64: `UNALIGNED_ACCESS_IS_FAST`, `machine_word_t = u64`.
const BITBUF_NBITS: u32 = 64;
const MAX_BITSLEFT: u32 = BITBUF_NBITS - 1;
const CONSUMABLE_NBITS: u32 = MAX_BITSLEFT - 7;
const PRELOAD_SLACK: u32 = 1;
const WORDBYTES: usize = 8;

const PRECODE_TABLEBITS: u32 = 7;
const PRECODE_ENOUGH: usize = 128; // enough 19 7 7
const LITLEN_TABLEBITS: u32 = 11;
const LITLEN_ENOUGH: usize = 2342; // enough 288 11 15
const OFFSET_TABLEBITS: u32 = 8;
const OFFSET_ENOUGH: usize = 402; // enough 32 8 15

/// Maximum number of bits that can be consumed by decoding a match length
const LENGTH_MAXBITS: u32 = DEFLATE_MAX_LITLEN_CODEWORD_LEN + DEFLATE_MAX_EXTRA_LENGTH_BITS;
/// Maximum number of bits that can be consumed by decoding a match offset
const OFFSET_MAXBITS: u32 = DEFLATE_MAX_OFFSET_CODEWORD_LEN + DEFLATE_MAX_EXTRA_OFFSET_BITS;
const OFFSET_MAXFASTBITS: u32 = OFFSET_TABLEBITS + DEFLATE_MAX_EXTRA_OFFSET_BITS;

/// The worst-case maximum number of output bytes written during each iteration of the
/// fastloop: 2 literals, then a match of length DEFLATE_MAX_MATCH_LEN, plus slack for the
/// intentional overrun in the match copy.
const FASTLOOP_MAX_BYTES_WRITTEN: usize = 2 + DEFLATE_MAX_MATCH_LEN + (5 * WORDBYTES) - 1;

/// The worst-case maximum number of input bytes read during each iteration of the fastloop.
const FASTLOOP_MAX_BYTES_READ: usize =
    ((MAX_BITSLEFT + (2 * LITLEN_TABLEBITS) + LENGTH_MAXBITS + OFFSET_MAXBITS) as usize)
        .div_ceil(8)
        + 8;

// Litlen and offset decode table entry flags
/// Indicates a literal entry in the litlen decode table
const HUFFDEC_LITERAL: u32 = 0x8000_0000;
/// Indicates that HUFFDEC_SUBTABLE_POINTER or HUFFDEC_END_OF_BLOCK is set
const HUFFDEC_EXCEPTIONAL: u32 = 0x0000_8000;
/// Indicates a subtable pointer entry in the litlen or offset decode table
const HUFFDEC_SUBTABLE_POINTER: u32 = 0x0000_4000;
/// Indicates an end-of-block entry in the litlen decode table
const HUFFDEC_END_OF_BLOCK: u32 = 0x0000_2000;

/// `BITMASK(n)`: a bitmask of length `n`.
#[inline]
fn bitmask(n: u32) -> u64 {
    (1u64 << n).wrapping_sub(1)
}

/// Port of `precode_decode_results` (external/deflate/lib/deflate_decompress.c:407-415
/// @ libdeflate v1.26): bit 20-16 presym.
const PRECODE_DECODE_RESULTS: [u32; DEFLATE_NUM_PRECODE_SYMS] = {
    let mut r = [0u32; DEFLATE_NUM_PRECODE_SYMS];
    let mut i = 0;
    while i < DEFLATE_NUM_PRECODE_SYMS {
        r[i] = (i as u32) << 16;
        i += 1;
    }
    r
};

/// Port of `litlen_decode_results` (external/deflate/lib/deflate_decompress.c:504-589
/// @ libdeflate v1.26): literals, end of block, then lengths (base value, extra bits).
const LITLEN_DECODE_RESULTS: [u32; DEFLATE_NUM_LITLEN_SYMS] = {
    let mut r = [0u32; DEFLATE_NUM_LITLEN_SYMS];
    let mut i = 0;
    // Literals
    while i < 256 {
        r[i] = HUFFDEC_LITERAL | ((i as u32) << 16);
        i += 1;
    }
    // End of block
    r[256] = HUFFDEC_EXCEPTIONAL | HUFFDEC_END_OF_BLOCK;
    // Lengths
    let lengths: [(u32, u32); 31] = [
        (3, 0),
        (4, 0),
        (5, 0),
        (6, 0),
        (7, 0),
        (8, 0),
        (9, 0),
        (10, 0),
        (11, 1),
        (13, 1),
        (15, 1),
        (17, 1),
        (19, 2),
        (23, 2),
        (27, 2),
        (31, 2),
        (35, 3),
        (43, 3),
        (51, 3),
        (59, 3),
        (67, 4),
        (83, 4),
        (99, 4),
        (115, 4),
        (131, 5),
        (163, 5),
        (195, 5),
        (227, 5),
        (258, 0),
        (258, 0),
        (258, 0),
    ];
    let mut k = 0;
    while k < 31 {
        r[257 + k] = (lengths[k].0 << 16) | lengths[k].1;
        k += 1;
    }
    r
};

/// Port of `offset_decode_results` (external/deflate/lib/deflate_decompress.c:617-629
/// @ libdeflate v1.26): offset base value, extra bits.
const OFFSET_DECODE_RESULTS: [u32; DEFLATE_NUM_OFFSET_SYMS] = {
    let offsets: [(u32, u32); 32] = [
        (1, 0),
        (2, 0),
        (3, 0),
        (4, 0),
        (5, 1),
        (7, 1),
        (9, 2),
        (13, 2),
        (17, 3),
        (25, 3),
        (33, 4),
        (49, 4),
        (65, 5),
        (97, 5),
        (129, 6),
        (193, 6),
        (257, 7),
        (385, 7),
        (513, 8),
        (769, 8),
        (1025, 9),
        (1537, 9),
        (2049, 10),
        (3073, 10),
        (4097, 11),
        (6145, 11),
        (8193, 12),
        (12289, 12),
        (16385, 13),
        (24577, 13),
        (24577, 13),
        (24577, 13),
    ];
    let mut r = [0u32; 32];
    let mut k = 0;
    while k < 32 {
        r[k] = (offsets[k].0 << 16) | offsets[k].1;
        k += 1;
    }
    r
};

/// Port of `make_decode_table_entry` (external/deflate/lib/deflate_decompress.c:387-391
/// @ libdeflate v1.26).
#[inline]
fn make_decode_table_entry(decode_results: &[u32], sym: u32, len: u32) -> u32 {
    decode_results[sym as usize]
        .wrapping_add(len << 8)
        .wrapping_add(len)
}

/// `bsr32`: the index of the highest set bit.
#[inline]
fn bsr32(v: u32) -> u32 {
    31 - v.leading_zeros()
}

/// Port of `build_decode_table` (external/deflate/lib/deflate_decompress.c:721-1004
/// @ libdeflate v1.26): the decode table of the canonical Huffman code that `lens` describes,
/// or `false` if the lengths do not form a valid code.
#[allow(clippy::too_many_arguments)]
fn build_decode_table(
    decode_table: &mut [u32],
    lens: &[u8],
    num_syms: usize,
    decode_results: &[u32],
    mut table_bits: u32,
    mut max_codeword_len: u32,
    sorted_syms: &mut [u16],
    table_bits_ret: Option<&mut u32>,
) -> bool {
    let mut len_counts = [0u32; DEFLATE_MAX_CODEWORD_LEN + 1];
    let mut offsets = [0u32; DEFLATE_MAX_CODEWORD_LEN + 1];

    // Count how many codewords have each length, including 0.
    for &l in &lens[..num_syms] {
        len_counts[l as usize] += 1;
    }

    // Determine the actual maximum codeword length that was used, and decrease table_bits to
    // it if allowed.
    while max_codeword_len > 1 && len_counts[max_codeword_len as usize] == 0 {
        max_codeword_len -= 1;
    }
    if let Some(r) = table_bits_ret {
        table_bits = table_bits.min(max_codeword_len);
        *r = table_bits;
    }

    // Sort the symbols primarily by increasing codeword length and secondarily by increasing
    // symbol value; also compute 'codespace_used' in the same pass.
    offsets[0] = 0;
    offsets[1] = len_counts[0];
    let mut codespace_used: u32 = 0;
    let mut len: u32 = 1;
    while len < max_codeword_len {
        offsets[len as usize + 1] = offsets[len as usize] + len_counts[len as usize];
        codespace_used = (codespace_used << 1) + len_counts[len as usize];
        len += 1;
    }
    codespace_used = (codespace_used << 1) + len_counts[len as usize];

    for sym in 0..num_syms {
        let l = lens[sym] as usize;
        sorted_syms[offsets[l] as usize] = sym as u16;
        offsets[l] += 1;
    }

    // Skip unused symbols
    let mut ss = offsets[0] as usize;

    // overfull code?
    if codespace_used > (1u32 << max_codeword_len) {
        return false;
    }

    // incomplete code?
    if codespace_used < (1u32 << max_codeword_len) {
        // The DEFLATE RFC explicitly allows the offset code to be incomplete in two cases: a
        // code containing just 1 codeword, if that codeword has length 1; and a code
        // containing no codewords. We accept what zlib accepts, plus a bit more, mapping each
        // allowed incomplete code to a complete code with only real symbols.
        let sym = if codespace_used == 0 {
            0 // arbitrary
        } else {
            if codespace_used != (1u32 << (max_codeword_len - 1)) || len_counts[1] != 1 {
                return false;
            }
            u32::from(sorted_syms[ss])
        };
        let entry = make_decode_table_entry(decode_results, sym, 1);
        for e in &mut decode_table[..1usize << table_bits] {
            *e = entry;
        }
        return true;
    }

    // The lengths form a complete code. Enumerate the codewords in lexicographic order and
    // fill the decode table entries for each one, doubling the table as the length grows.
    let mut codeword: u32 = 0;
    len = 1;
    let mut count;
    loop {
        count = len_counts[len as usize];
        if count != 0 {
            break;
        }
        len += 1;
    }
    let mut cur_table_end: u32 = 1u32 << len;
    while len <= table_bits {
        // Process all 'count' codewords with length 'len' bits.
        loop {
            // Fill the first entry for the current codeword.
            decode_table[codeword as usize] =
                make_decode_table_entry(decode_results, u32::from(sorted_syms[ss]), len);
            ss += 1;

            if codeword == cur_table_end - 1 {
                // Last codeword (all 1's)
                while len < table_bits {
                    let n = cur_table_end as usize;
                    decode_table.copy_within(0..n, n);
                    cur_table_end <<= 1;
                    len += 1;
                }
                return true;
            }
            // Advance to the lexicographically next codeword in the canonical code.
            let bit = 1u32 << bsr32(codeword ^ (cur_table_end - 1));
            codeword &= bit - 1;
            codeword |= bit;
            count -= 1;
            if count == 0 {
                break;
            }
        }

        // Advance to the next codeword length.
        loop {
            len += 1;
            if len <= table_bits {
                let n = cur_table_end as usize;
                decode_table.copy_within(0..n, n);
                cur_table_end <<= 1;
            }
            count = len_counts[len as usize];
            if count != 0 {
                break;
            }
        }
    }

    // Process codewords with len > table_bits. These require subtables.
    cur_table_end = 1u32 << table_bits;
    let mut subtable_prefix: u32 = u32::MAX;
    let mut subtable_start: u32 = 0;
    loop {
        // Start a new subtable if the first 'table_bits' bits of the codeword don't match the
        // prefix of the current subtable.
        if (codeword & ((1u32 << table_bits) - 1)) != subtable_prefix {
            subtable_prefix = codeword & ((1u32 << table_bits) - 1);
            subtable_start = cur_table_end;
            // Calculate the subtable length.
            let mut subtable_bits = len - table_bits;
            codespace_used = count;
            while codespace_used < (1u32 << subtable_bits) {
                subtable_bits += 1;
                codespace_used =
                    (codespace_used << 1) + len_counts[(table_bits + subtable_bits) as usize];
            }
            cur_table_end = subtable_start + (1u32 << subtable_bits);

            // Create the entry that points from the main table to the subtable.
            decode_table[subtable_prefix as usize] = (subtable_start << 16)
                | HUFFDEC_EXCEPTIONAL
                | HUFFDEC_SUBTABLE_POINTER
                | (subtable_bits << 8)
                | table_bits;
        }

        // Fill the subtable entries for the current codeword.
        let entry =
            make_decode_table_entry(decode_results, u32::from(sorted_syms[ss]), len - table_bits);
        ss += 1;
        let mut i = subtable_start + (codeword >> table_bits);
        let stride = 1u32 << (len - table_bits);
        loop {
            decode_table[i as usize] = entry;
            i += stride;
            if i >= cur_table_end {
                break;
            }
        }

        // Advance to the next codeword.
        if codeword == (1u32 << len) - 1 {
            // last codeword (all 1's)
            return true;
        }
        let bit = 1u32 << bsr32(codeword ^ ((1u32 << len) - 1));
        codeword &= bit - 1;
        codeword |= bit;
        count -= 1;
        while count == 0 {
            len += 1;
            count = len_counts[len as usize];
        }
    }
}

/// Port of `struct libdeflate_decompressor` (external/deflate/lib/deflate_decompress.c:642-677
/// @ libdeflate v1.26): the decode tables and the arrays used to build them, kept across
/// streams so that the static codes' tables are reused.
#[derive(Clone)]
pub struct Decompressor {
    precode_lens: [u8; DEFLATE_NUM_PRECODE_SYMS],
    lens: [u8; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS + DEFLATE_MAX_LENS_OVERRUN],
    precode_decode_table: [u32; PRECODE_ENOUGH],
    litlen_decode_table: [u32; LITLEN_ENOUGH],
    offset_decode_table: [u32; OFFSET_ENOUGH],
    // used only during build_decode_table()
    sorted_syms: [u16; DEFLATE_MAX_NUM_SYMS],
    static_codes_loaded: bool,
    litlen_tablebits: u32,
}

impl std::fmt::Debug for Decompressor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decompressor")
            .field("static_codes_loaded", &self.static_codes_loaded)
            .field("litlen_tablebits", &self.litlen_tablebits)
            .finish_non_exhaustive()
    }
}

impl Default for Decompressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Decompressor {
    /// Port of `libdeflate_alloc_decompressor` (external/deflate/lib/deflate_decompress.c:1155-1201
    /// @ libdeflate v1.26): "for simplicity, we currently just zero the whole decompressor".
    pub fn new() -> Decompressor {
        Decompressor {
            precode_lens: [0; DEFLATE_NUM_PRECODE_SYMS],
            lens: [0; DEFLATE_NUM_LITLEN_SYMS + DEFLATE_NUM_OFFSET_SYMS + DEFLATE_MAX_LENS_OVERRUN],
            precode_decode_table: [0; PRECODE_ENOUGH],
            litlen_decode_table: [0; LITLEN_ENOUGH],
            offset_decode_table: [0; OFFSET_ENOUGH],
            sorted_syms: [0; DEFLATE_MAX_NUM_SYMS],
            static_codes_loaded: false,
            litlen_tablebits: 0,
        }
    }

    /// Port of `build_precode_decode_table` (external/deflate/lib/deflate_decompress.c:1007-1024
    /// @ libdeflate v1.26).
    fn build_precode_decode_table(&mut self) -> bool {
        build_decode_table(
            &mut self.precode_decode_table,
            &self.precode_lens,
            DEFLATE_NUM_PRECODE_SYMS,
            &PRECODE_DECODE_RESULTS,
            PRECODE_TABLEBITS,
            DEFLATE_MAX_PRE_CODEWORD_LEN,
            &mut self.sorted_syms,
            None,
        )
    }

    /// Port of `build_litlen_decode_table` (external/deflate/lib/deflate_decompress.c:1027-1045
    /// @ libdeflate v1.26).
    fn build_litlen_decode_table(&mut self, num_litlen_syms: usize) -> bool {
        build_decode_table(
            &mut self.litlen_decode_table,
            &self.lens,
            num_litlen_syms,
            &LITLEN_DECODE_RESULTS,
            LITLEN_TABLEBITS,
            DEFLATE_MAX_LITLEN_CODEWORD_LEN,
            &mut self.sorted_syms,
            Some(&mut self.litlen_tablebits),
        )
    }

    /// Port of `build_offset_decode_table` (external/deflate/lib/deflate_decompress.c:1048-1066
    /// @ libdeflate v1.26).
    fn build_offset_decode_table(
        &mut self,
        num_litlen_syms: usize,
        num_offset_syms: usize,
    ) -> bool {
        build_decode_table(
            &mut self.offset_decode_table,
            &self.lens[num_litlen_syms..],
            num_offset_syms,
            &OFFSET_DECODE_RESULTS,
            OFFSET_TABLEBITS,
            DEFLATE_MAX_OFFSET_CODEWORD_LEN,
            &mut self.sorted_syms,
            None,
        )
    }

    /// Port of `libdeflate_deflate_decompress` (external/deflate/lib/deflate_decompress.c:1144-1153
    /// @ libdeflate v1.26).
    pub fn deflate_decompress(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        actual_out_nbytes_ret: Option<&mut usize>,
    ) -> LibdeflateResult {
        self.deflate_decompress_ex(input, out, None, actual_out_nbytes_ret)
    }

    /// Port of `libdeflate_deflate_decompress_ex` (external/deflate/lib/deflate_decompress.c:1133-1142
    /// @ libdeflate v1.26), with the body of `decompress_template.h` (lines 44-772): decompresses
    /// the DEFLATE stream `input` into `out`. Without `actual_out_nbytes_ret`, a stream that
    /// does not fill `out` is `ShortOutput`.
    pub fn deflate_decompress_ex(
        &mut self,
        input: &[u8],
        out: &mut [u8],
        actual_in_nbytes_ret: Option<&mut usize>,
        actual_out_nbytes_ret: Option<&mut usize>,
    ) -> LibdeflateResult {
        let d = self;
        let out_end = out.len();
        let out_fastloop_end = out_end - out_end.min(FASTLOOP_MAX_BYTES_WRITTEN);
        let in_end = input.len();
        let in_fastloop_end = in_end - in_end.min(FASTLOOP_MAX_BYTES_READ);
        let mut out_next: usize = 0;

        // Input bitstream state; see deflate_decompress.c for documentation
        let mut in_next: usize = 0;
        let mut bitbuf: u64 = 0;
        let mut saved_bitbuf: u64;
        let mut bitsleft: u32 = 0;
        let mut overread_count: usize = 0;

        let mut entry: u32;

        macro_rules! safety_check {
            ($e:expr) => {
                if !($e) {
                    return LibdeflateResult::BadData;
                }
            };
        }
        // REFILL_BITS_BRANCHLESS()
        macro_rules! refill_bits_branchless {
            () => {
                let w =
                    u64::from_le_bytes(input[in_next..in_next + 8].try_into().expect("8 bytes"));
                bitbuf |= w << u32::from(bitsleft as u8);
                in_next += 8 - 1;
                in_next -= ((bitsleft >> 3) & 0x7) as usize;
                bitsleft |= MAX_BITSLEFT & !7;
            };
        }
        // REFILL_BITS()
        macro_rules! refill_bits {
            () => {
                if in_end - in_next >= 8 {
                    refill_bits_branchless!();
                } else {
                    while (bitsleft as u8 as u32) < CONSUMABLE_NBITS {
                        if in_next != in_end {
                            bitbuf |= u64::from(input[in_next]) << u32::from(bitsleft as u8);
                            in_next += 1;
                        } else {
                            overread_count += 1;
                            safety_check!(overread_count <= 8);
                        }
                        bitsleft = bitsleft.wrapping_add(8);
                    }
                }
            };
        }
        // Consumes the bits of a decode table entry: 'bitsleft -= entry' subtracts the full
        // entry (the high bits of bitsleft may hold garbage).
        macro_rules! consume {
            ($e:expr) => {
                bitbuf >>= u32::from($e as u8);
                bitsleft = bitsleft.wrapping_sub($e);
            };
        }
        // a word-at-a-time copy within the output: load_word_unaligned + store_word_unaligned
        macro_rules! copy_word {
            ($src:expr, $dst:expr) => {
                out.copy_within($src..$src + WORDBYTES, $dst);
            };
        }

        'blocks: loop {
            // next_block: Starting to read the next block
            refill_bits!();

            // BFINAL: 1 bit
            let is_final_block = bitbuf & bitmask(1) != 0;
            // BTYPE: 2 bits
            let block_type = ((bitbuf >> 1) & bitmask(2)) as u32;

            let mut num_litlen_syms = 0usize;
            let mut num_offset_syms = 0usize;
            let mut have_tables = false;
            let mut block_done = false;

            if block_type == DEFLATE_BLOCKTYPE_DYNAMIC_HUFFMAN {
                // The order in which precode lengths are stored
                const PERMUTATION: [u8; DEFLATE_NUM_PRECODE_SYMS] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];

                // Read the codeword length counts.
                num_litlen_syms = 257 + ((bitbuf >> 3) & bitmask(5)) as usize;
                num_offset_syms = 1 + ((bitbuf >> 8) & bitmask(5)) as usize;
                let num_explicit_precode_lens = 4 + ((bitbuf >> 13) & bitmask(4)) as usize;

                d.static_codes_loaded = false;

                // Read the precode codeword lengths. A 64-bit bitbuffer is just one bit too
                // small to hold the maximum number of precode lens, so to minimize branches
                // we merge one len with the previous fields.
                d.precode_lens[PERMUTATION[0] as usize] = ((bitbuf >> 17) & bitmask(3)) as u8;
                bitbuf >>= 20;
                bitsleft = bitsleft.wrapping_sub(20);
                refill_bits!();
                let mut i = 1usize;
                loop {
                    d.precode_lens[PERMUTATION[i] as usize] = (bitbuf & bitmask(3)) as u8;
                    bitbuf >>= 3;
                    bitsleft = bitsleft.wrapping_sub(3);
                    i += 1;
                    if i >= num_explicit_precode_lens {
                        break;
                    }
                }
                while i < DEFLATE_NUM_PRECODE_SYMS {
                    d.precode_lens[PERMUTATION[i] as usize] = 0;
                    i += 1;
                }

                // Build the decode table for the precode.
                safety_check!(d.build_precode_decode_table());

                // Decode the litlen and offset codeword lengths.
                let mut i = 0usize;
                loop {
                    if (bitsleft as u8 as u32) < DEFLATE_MAX_PRE_CODEWORD_LEN + 7 {
                        refill_bits!();
                    }

                    // Decode the next precode symbol.
                    entry = d.precode_decode_table
                        [(bitbuf & bitmask(DEFLATE_MAX_PRE_CODEWORD_LEN)) as usize];
                    consume!(entry);
                    let presym = entry >> 16;

                    if presym < 16 {
                        // Explicit codeword length
                        d.lens[i] = presym as u8;
                        i += 1;
                    } else if presym == 16 {
                        // Run-length encoded codeword lengths. The lens array is sized for the
                        // worst-case overrun, so the small repeat counts always write the
                        // maximum number of entries.
                        // Repeat the previous length 3 - 6 times.
                        safety_check!(i != 0);
                        let rep_val = d.lens[i - 1];
                        let rep_count = 3 + (bitbuf & bitmask(2)) as usize;
                        bitbuf >>= 2;
                        bitsleft = bitsleft.wrapping_sub(2);
                        d.lens[i..i + 6].fill(rep_val);
                        i += rep_count;
                    } else if presym == 17 {
                        // Repeat zero 3 - 10 times.
                        let rep_count = 3 + (bitbuf & bitmask(3)) as usize;
                        bitbuf >>= 3;
                        bitsleft = bitsleft.wrapping_sub(3);
                        d.lens[i..i + 10].fill(0);
                        i += rep_count;
                    } else {
                        // Repeat zero 11 - 138 times.
                        let rep_count = 11 + (bitbuf & bitmask(7)) as usize;
                        bitbuf >>= 7;
                        bitsleft = bitsleft.wrapping_sub(7);
                        d.lens[i..i + rep_count].fill(0);
                        i += rep_count;
                    }
                    if i >= num_litlen_syms + num_offset_syms {
                        break;
                    }
                }

                // Unnecessary, but check this for consistency with zlib.
                safety_check!(i == num_litlen_syms + num_offset_syms);
            } else if block_type == DEFLATE_BLOCKTYPE_UNCOMPRESSED {
                // Uncompressed block: copy 'len' bytes literally from the input buffer to the
                // output buffer.
                bitsleft = bitsleft.wrapping_sub(3); // for BTYPE and BFINAL

                // Align the bitstream to the next byte boundary, rewinding 'in_next' by any
                // bytes that have been refilled but not actually consumed yet (not counting
                // overread bytes, which don't increment 'in_next').
                bitsleft = u32::from(bitsleft as u8);
                safety_check!(overread_count <= (bitsleft >> 3) as usize);
                in_next -= (bitsleft >> 3) as usize - overread_count;
                overread_count = 0;
                bitbuf = 0;
                bitsleft = 0;

                safety_check!(in_end - in_next >= 4);
                let len = u16::from_le_bytes([input[in_next], input[in_next + 1]]);
                let nlen = u16::from_le_bytes([input[in_next + 2], input[in_next + 3]]);
                in_next += 4;

                safety_check!(len == !nlen);
                let len = len as usize;
                if len > out_end - out_next {
                    return LibdeflateResult::InsufficientSpace;
                }
                safety_check!(len <= in_end - in_next);

                out[out_next..out_next + len].copy_from_slice(&input[in_next..in_next + len]);
                in_next += len;
                out_next += len;

                block_done = true;
            } else {
                safety_check!(block_type == DEFLATE_BLOCKTYPE_STATIC_HUFFMAN);

                // Static Huffman block: build the decode tables for the static codes, unless
                // they are already set up from an earlier static block.
                bitbuf >>= 3; // for BTYPE and BFINAL
                bitsleft = bitsleft.wrapping_sub(3);

                if d.static_codes_loaded {
                    have_tables = true;
                } else {
                    d.static_codes_loaded = true;
                    d.lens[..144].fill(8);
                    d.lens[144..256].fill(9);
                    d.lens[256..280].fill(7);
                    d.lens[280..288].fill(8);
                    d.lens[288..288 + 32].fill(5);
                    num_litlen_syms = 288;
                    num_offset_syms = 32;
                }
            }

            if !block_done {
                // Decompressing a Huffman block (either dynamic or static)
                if !have_tables {
                    safety_check!(d.build_offset_decode_table(num_litlen_syms, num_offset_syms));
                    safety_check!(d.build_litlen_decode_table(num_litlen_syms));
                }
                // have_decode_tables:
                let litlen_tablemask = bitmask(d.litlen_tablebits);

                // The "fastloop" for decoding literals and matches. Its loop conditions bound
                // in_next and out_next, so the body needs no other bounds checks.
                if !(in_next >= in_fastloop_end || out_next >= out_fastloop_end) {
                    refill_bits_branchless!();
                    entry = d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                    'fast: loop {
                        // a `continue` of the C do-while: on to the loop condition
                        'item: {
                            // Consume the bits for the litlen decode table entry. Save the
                            // original bitbuf for later, in case the extra match length bits
                            // need to be extracted from it.
                            saved_bitbuf = bitbuf;
                            consume!(entry);

                            // Begin by checking for a "fast" literal, i.e. a literal that
                            // doesn't need a subtable. On 64-bit platforms, up to 2 extra fast
                            // literals are decoded in addition to the primary item.
                            if entry & HUFFDEC_LITERAL != 0 {
                                // 1st extra fast literal
                                let lit = (entry >> 16) as u8;
                                entry = d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                                saved_bitbuf = bitbuf;
                                consume!(entry);
                                out[out_next] = lit;
                                out_next += 1;
                                if entry & HUFFDEC_LITERAL != 0 {
                                    // 2nd extra fast literal
                                    let lit = (entry >> 16) as u8;
                                    entry =
                                        d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                                    saved_bitbuf = bitbuf;
                                    consume!(entry);
                                    out[out_next] = lit;
                                    out_next += 1;
                                    if entry & HUFFDEC_LITERAL != 0 {
                                        // Another fast literal, but this one is in lieu of the
                                        // primary item, so it doesn't count as one of the
                                        // extras.
                                        let lit = (entry >> 16) as u8;
                                        entry = d.litlen_decode_table
                                            [(bitbuf & litlen_tablemask) as usize];
                                        refill_bits_branchless!();
                                        out[out_next] = lit;
                                        out_next += 1;
                                        break 'item;
                                    }
                                }
                            }

                            // It's not a literal entry, so it can be a length entry, a subtable
                            // pointer entry, or an end-of-block entry.
                            if entry & HUFFDEC_EXCEPTIONAL != 0 {
                                // Subtable pointer or end-of-block entry
                                if entry & HUFFDEC_END_OF_BLOCK != 0 {
                                    block_done = true;
                                    break 'fast;
                                }

                                // A subtable is required. Load and consume the subtable entry,
                                // of any type: literal, length, or end-of-block.
                                entry = d.litlen_decode_table[((entry >> 16) as u64
                                    + (bitbuf & bitmask((entry >> 8) & 0x3F)))
                                    as usize];
                                saved_bitbuf = bitbuf;
                                consume!(entry);

                                if entry & HUFFDEC_LITERAL != 0 {
                                    // Decode a literal that required a subtable.
                                    let lit = (entry >> 16) as u8;
                                    entry =
                                        d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                                    refill_bits_branchless!();
                                    out[out_next] = lit;
                                    out_next += 1;
                                    break 'item;
                                }
                                if entry & HUFFDEC_END_OF_BLOCK != 0 {
                                    block_done = true;
                                    break 'fast;
                                }
                                // Else, it's a length that required a subtable.
                            }

                            // Decode the match length: the length base value plus the extra
                            // length bits, already consumed with the entry.
                            let mut length = entry >> 16;
                            length += ((saved_bitbuf & bitmask(u32::from(entry as u8)))
                                >> u32::from((entry >> 8) as u8))
                                as u32;

                            // Decode the match offset. We may need to refill once, but then we
                            // can decode the whole offset and preload the next litlen entry.
                            entry = d.offset_decode_table
                                [(bitbuf & bitmask(OFFSET_TABLEBITS)) as usize];
                            if entry & HUFFDEC_EXCEPTIONAL != 0 {
                                // Offset codeword requires a subtable
                                if (bitsleft as u8 as u32)
                                    < OFFSET_MAXBITS + LITLEN_TABLEBITS - PRELOAD_SLACK
                                {
                                    refill_bits_branchless!();
                                }
                                bitbuf >>= OFFSET_TABLEBITS;
                                bitsleft = bitsleft.wrapping_sub(OFFSET_TABLEBITS);
                                entry = d.offset_decode_table[((entry >> 16) as u64
                                    + (bitbuf & bitmask((entry >> 8) & 0x3F)))
                                    as usize];
                            } else if (bitsleft as u8 as u32)
                                < OFFSET_MAXFASTBITS + LITLEN_TABLEBITS - PRELOAD_SLACK
                            {
                                refill_bits_branchless!();
                            }
                            saved_bitbuf = bitbuf;
                            consume!(entry);
                            let mut offset = entry >> 16;
                            offset += ((saved_bitbuf & bitmask(u32::from(entry as u8)))
                                >> u32::from((entry >> 8) as u8))
                                as u32;

                            // Validate the match offset; needed even in the fastloop.
                            safety_check!(offset as usize <= out_next);
                            let offset = offset as usize;
                            let mut src = out_next - offset;
                            let mut dst = out_next;
                            out_next += length as usize;

                            // Before copying the match, refill the bitbuffer and preload the
                            // litlen decode table entry for the next loop iteration.
                            entry = d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                            refill_bits_branchless!();

                            // Copy the match a word at a time, writing past the match 'length'
                            // (there is output space left for the overrun).
                            if offset >= WORDBYTES {
                                for _ in 0..5 {
                                    copy_word!(src, dst);
                                    src += WORDBYTES;
                                    dst += WORDBYTES;
                                }
                                while dst < out_next {
                                    for _ in 0..5 {
                                        copy_word!(src, dst);
                                        src += WORDBYTES;
                                        dst += WORDBYTES;
                                    }
                                }
                            } else if offset == 1 {
                                let v = (0x0101_0101_0101_0101u64
                                    .wrapping_mul(u64::from(out[src])))
                                .to_ne_bytes();
                                for _ in 0..4 {
                                    out[dst..dst + WORDBYTES].copy_from_slice(&v);
                                    dst += WORDBYTES;
                                }
                                while dst < out_next {
                                    for _ in 0..4 {
                                        out[dst..dst + WORDBYTES].copy_from_slice(&v);
                                        dst += WORDBYTES;
                                    }
                                }
                            } else {
                                copy_word!(src, dst);
                                src += offset;
                                dst += offset;
                                copy_word!(src, dst);
                                src += offset;
                                dst += offset;
                                loop {
                                    copy_word!(src, dst);
                                    src += offset;
                                    dst += offset;
                                    copy_word!(src, dst);
                                    src += offset;
                                    dst += offset;
                                    if dst >= out_next {
                                        break;
                                    }
                                }
                            }
                        }
                        if !(in_next < in_fastloop_end && out_next < out_fastloop_end) {
                            break 'fast;
                        }
                    }
                }

                // The generic loop for decoding literals and matches, near the ends of the
                // buffers.
                if !block_done {
                    loop {
                        refill_bits!();
                        entry = d.litlen_decode_table[(bitbuf & litlen_tablemask) as usize];
                        saved_bitbuf = bitbuf;
                        consume!(entry);
                        if entry & HUFFDEC_SUBTABLE_POINTER != 0 {
                            entry = d.litlen_decode_table[((entry >> 16) as u64
                                + (bitbuf & bitmask((entry >> 8) & 0x3F)))
                                as usize];
                            saved_bitbuf = bitbuf;
                            consume!(entry);
                        }
                        let mut length = entry >> 16;
                        if entry & HUFFDEC_LITERAL != 0 {
                            if out_next == out_end {
                                return LibdeflateResult::InsufficientSpace;
                            }
                            out[out_next] = length as u8;
                            out_next += 1;
                            continue;
                        }
                        if entry & HUFFDEC_END_OF_BLOCK != 0 {
                            break;
                        }
                        length += ((saved_bitbuf & bitmask(u32::from(entry as u8)))
                            >> u32::from((entry >> 8) as u8))
                            as u32;
                        if length as usize > out_end - out_next {
                            return LibdeflateResult::InsufficientSpace;
                        }

                        entry =
                            d.offset_decode_table[(bitbuf & bitmask(OFFSET_TABLEBITS)) as usize];
                        if entry & HUFFDEC_EXCEPTIONAL != 0 {
                            bitbuf >>= OFFSET_TABLEBITS;
                            bitsleft = bitsleft.wrapping_sub(OFFSET_TABLEBITS);
                            entry = d.offset_decode_table[((entry >> 16) as u64
                                + (bitbuf & bitmask((entry >> 8) & 0x3F)))
                                as usize];
                        }
                        let mut offset = entry >> 16;
                        offset += ((bitbuf & bitmask(u32::from(entry as u8)))
                            >> u32::from((entry >> 8) as u8))
                            as u32;
                        consume!(entry);

                        safety_check!(offset as usize <= out_next);
                        let mut src = out_next - offset as usize;
                        let mut dst = out_next;
                        out_next += length as usize;

                        out[dst] = out[src];
                        dst += 1;
                        src += 1;
                        out[dst] = out[src];
                        dst += 1;
                        src += 1;
                        loop {
                            out[dst] = out[src];
                            dst += 1;
                            src += 1;
                            if dst >= out_next {
                                break;
                            }
                        }
                    }
                }
            }

            // block_done: Finished decoding a block
            if !is_final_block {
                continue 'blocks;
            }
            break;
        }

        // That was the last block.
        bitsleft = u32::from(bitsleft as u8);

        // If any of the implicit appended zero bytes were consumed (not just refilled) before
        // hitting end of stream, then the data is bad.
        safety_check!(overread_count <= (bitsleft >> 3) as usize);

        // Optionally return the actual number of bytes consumed.
        if let Some(r) = actual_in_nbytes_ret {
            // Don't count bytes that were refilled but not consumed.
            in_next -= (bitsleft >> 3) as usize - overread_count;
            *r = in_next;
        }

        // Optionally return the actual number of bytes written.
        match actual_out_nbytes_ret {
            Some(r) => *r = out_next,
            None => {
                if out_next != out_end {
                    return LibdeflateResult::ShortOutput;
                }
            }
        }
        LibdeflateResult::Success
    }
}
