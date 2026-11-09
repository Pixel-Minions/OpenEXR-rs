// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_huf.c` (src/lib/OpenEXRCore/internal_huf.c:1-2000 @ v3.5.3): the Huffman
//! coder of PIZ (and of DWA's AC coefficients): 16-bit symbols, code lengths up to 58 bits, a
//! run-length pseudo-symbol; the classic decoder and the fast table-driven one (which x86-64
//! builds of both wheels use for inputs of more than 128 bits).
//!
//! Upstream partitions a caller's spare buffer into its work arrays; the port allocates them
//! and keeps the size check (`EXR_ERR_INVALID_ARGUMENT` for a smaller spare buffer). The sizes
//! are those of x86-64, where both wheels run: 8-byte pointers, and `FastHufDecoder` laid out
//! as C lays it out (the `#[repr(C)]` copy below). The heap of the encoder holds pointers into
//! the frequency table upstream; here it holds their indices, which compare as the pointers do.

use crate::buffer_pool::Kept;
use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::errors::ErrorCode;

/// Port of `HUF_ENCBITS` (src/lib/OpenEXRCore/internal_huf.c:18 @ v3.5.3): literal (value) bit
/// length.
const HUF_ENCBITS: usize = 16;
/// Port of `HUF_DECBITS` (src/lib/OpenEXRCore/internal_huf.c:19 @ v3.5.3): decoding bit size
/// (>= 8).
const HUF_DECBITS: usize = 14;
/// Port of `HUF_ENCSIZE` (src/lib/OpenEXRCore/internal_huf.c:21 @ v3.5.3): encoding table size.
const HUF_ENCSIZE: usize = (1 << HUF_ENCBITS) + 1;
/// Port of `HUF_DECSIZE` (src/lib/OpenEXRCore/internal_huf.c:22 @ v3.5.3): decoding table size.
const HUF_DECSIZE: usize = 1 << HUF_DECBITS;
/// Port of `HUF_DECMASK` (src/lib/OpenEXRCore/internal_huf.c:23 @ v3.5.3).
const HUF_DECMASK: u64 = (HUF_DECSIZE - 1) as u64;

// src/lib/OpenEXRCore/internal_huf.c:25-28 @ v3.5.3
const SHORT_ZEROCODE_RUN: u64 = 59;
const LONG_ZEROCODE_RUN: u64 = 63;
const SHORTEST_LONG_RUN: u64 = 2 + LONG_ZEROCODE_RUN - SHORT_ZEROCODE_RUN;
const LONGEST_LONG_RUN: u64 = 255 + SHORTEST_LONG_RUN;

/// Port of `MAX_CODE_LEN` (src/lib/OpenEXRCore/internal_huf.c:1043 @ v3.5.3): the longest
/// compressed code length that ImfHuf supports (58 bits).
const MAX_CODE_LEN: usize = 58;
/// Port of `TABLE_LOOKUP_BITS` (src/lib/OpenEXRCore/internal_huf.c:1050 @ v3.5.3): the number of
/// bits in the fast decoder's acceleration table.
const TABLE_LOOKUP_BITS: usize = 12;
/// Port of `INDEX_BIT_SHIFT` (src/lib/OpenEXRCore/internal_huf.c:1052 @ v3.5.3).
const INDEX_BIT_SHIFT: u32 = (64 - TABLE_LOOKUP_BITS) as u32;

/// `hufLength`: a code's length, its low 6 bits.
#[inline]
fn huf_length(code: u64) -> i32 {
    (code & 63) as i32
}

/// `hufCode`: a code's bits, above its length.
#[inline]
fn huf_code(code: u64) -> u64 {
    code >> 6
}

fn bare(code: ErrorCode) -> Error {
    Error::bare(code)
}

/// The layout of `HufDec` (src/lib/OpenEXRCore/internal_huf.c:50-55 @ v3.5.3), for its size.
#[allow(dead_code)]
#[repr(C)]
struct HufDecLayout {
    len: i32,
    lit: u32,
    /// `uint32_t* p`.
    p: usize,
}

/// Port of `HufDec` (src/lib/OpenEXRCore/internal_huf.c:50-55 @ v3.5.3): a decoding table
/// entry: a short code's length and symbol, or a long code's candidates (`p`, `lit` of them).
#[derive(Clone, Default)]
struct HufDec {
    len: i32,
    lit: u32,
    p: Option<Vec<u32>>,
}

/// The layout of `FastHufDecoder` (src/lib/OpenEXRCore/internal_huf.c:1084-1130 @ v3.5.3), for
/// its size.
#[allow(dead_code)]
#[repr(C)]
struct FastHufDecoderLayout {
    rle_symbol: i32,
    num_symbols: u32,
    min_code_length: u8,
    max_code_length: u8,
    pad: [u8; 2],
    id_to_symbol: [u32; 65536 + 1],
    lj_base: [u64; MAX_CODE_LEN + 1 + 1],
    lj_offset: [u64; MAX_CODE_LEN + 1],
    table_symbol: [i32; 1 << TABLE_LOOKUP_BITS],
    table_code_len: [u8; 1 << TABLE_LOOKUP_BITS],
    table_min: u64,
}

/// Port of `FastHufDecoder` (src/lib/OpenEXRCore/internal_huf.c:1084-1130 @ v3.5.3).
struct FastHufDecoder {
    /// RLE symbol written by the encoder. This could be 65536, so beware when you use shorts
    /// to hold things.
    rle_symbol: i32,
    /// Number of symbols in the codebook.
    num_symbols: u32,
    /// Minimum code length, in bits.
    min_code_length: u8,
    /// Maximum code length, in bits.
    max_code_length: u8,
    /// Maps Ids to symbols. Ids are a symbol ordering sorted first in terms of code length,
    /// and by code within the same length. Ids run from 0 to mNumSymbols-1.
    id_to_symbol: Kept<u32>,
    /// The 'left justified base' table, with a sentinel terminator.
    lj_base: [u64; MAX_CODE_LEN + 1 + 1],
    /// The 'left justified' offset constants, by code length.
    lj_offset: [u64; MAX_CODE_LEN + 1],
    table_symbol: Kept<i32>,
    table_code_len: Kept<u8>,
    table_min: u64,
}

/// Port of `internal_exr_huf_compress_spare_bytes` (src/lib/OpenEXRCore/internal_huf.c:1823-1832
/// @ v3.5.3).
pub(crate) fn internal_exr_huf_compress_spare_bytes() -> u64 {
    let mut ret: u64 = 0;
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // freq
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // scode
    ret += (HUF_ENCSIZE * size_of::<usize>()) as u64; // fheap
    ret += (HUF_ENCSIZE * size_of::<u32>()) as u64; // hlink
    ret
}

/// Port of `internal_exr_huf_decompress_spare_bytes` (src/lib/OpenEXRCore/internal_huf.c:1834-1844
/// @ v3.5.3).
pub(crate) fn internal_exr_huf_decompress_spare_bytes() -> u64 {
    let mut ret: u64 = 0;
    ret += (HUF_ENCSIZE * size_of::<u64>()) as u64; // freq
    ret += (HUF_DECSIZE * size_of::<HufDecLayout>()) as u64; // hdec
    if size_of::<FastHufDecoderLayout>() as u64 > ret {
        ret = size_of::<FastHufDecoderLayout>() as u64;
    }
    ret
}

/// `outputBits` and the output position of the encoder (src/lib/OpenEXRCore/internal_huf.c:98-109
/// @ v3.5.3).
struct BitOut<'o> {
    c: u64,
    lc: i32,
    out: &'o mut [u8],
    pos: usize,
}

impl BitOut<'_> {
    /// `outputBits`: appends `n_bits` bits, writing whole bytes; past the end of the output,
    /// `EXR_ERR_ARGUMENT_OUT_OF_RANGE`.
    #[inline]
    fn output_bits(&mut self, n_bits: i32, bits: u64) -> Result<(), Error> {
        self.c = self.c.wrapping_shl(n_bits as u32) | bits;
        self.lc += n_bits;
        while self.lc >= 8 {
            if self.pos >= self.out.len() {
                return Err(bare(ErrorCode::ArgumentOutOfRange));
            }
            self.lc -= 8;
            self.out[self.pos] = (self.c >> self.lc) as u8;
            self.pos += 1;
        }
        Ok(())
    }

    /// `outputCode` (src/lib/OpenEXRCore/internal_huf.c:742-743 @ v3.5.3).
    #[inline]
    fn output_code(&mut self, code: u64) -> Result<(), Error> {
        self.output_bits(huf_length(code), huf_code(code))
    }

    /// `sendCode` (src/lib/OpenEXRCore/internal_huf.c:776-790 @ v3.5.3): a symbol repeated
    /// `run_count` more times, as a run when that is shorter.
    #[inline]
    fn send_code(&mut self, s_code: u64, mut run_count: i32, run_code: u64) -> Result<(), Error> {
        if (huf_length(s_code) + huf_length(run_code) + 8) < (huf_length(s_code) * run_count) {
            self.output_code(s_code)?;
            self.output_code(run_code)?;
            self.output_bits(8, run_count as u64)?;
        } else {
            loop {
                let more = run_count >= 0;
                run_count -= 1;
                if !more {
                    break;
                }
                self.output_code(s_code)?;
            }
        }
        Ok(())
    }
}

/// Port of `getBits` (src/lib/OpenEXRCore/internal_huf.c:111-124 @ v3.5.3): the next `n_bits`
/// bits of `input` from `pos`.
#[inline]
fn get_bits(n_bits: u32, c: &mut u64, lc: &mut u32, input: &[u8], pos: &mut usize) -> u64 {
    while *lc < n_bits {
        *c = (*c << 8) | u64::from(input[*pos]);
        *pos += 1;
        *lc += 8;
    }
    *lc -= n_bits;
    (*c >> *lc) & ((1u64 << n_bits) - 1)
}

/// Port of `hufCanonicalCodeTable` (src/lib/OpenEXRCore/internal_huf.c:146-192 @ v3.5.3): the
/// canonical codes for the code lengths in `hcode`, each stored with its length.
fn huf_canonical_code_table(hcode: &mut [u64]) {
    let mut n = [0u64; 59];

    // For each i from 0 through 58, count the number of different codes of length i, and
    // store the count in n[i].
    for &h in &hcode[..HUF_ENCSIZE] {
        n[h as usize] += 1;
    }

    // For each i from 58 through 1, compute the numerically lowest code with length i, and
    // store that code in n[i].
    let mut c = 0u64;
    for i in (1..=58).rev() {
        let nc = (c + n[i]) >> 1;
        n[i] = c;
        c = nc;
    }

    // hcode[i] contains the length, l, of the code for symbol i. Assign the next available
    // code of length l to the symbol and store both l and the code in hcode[i].
    for h in &mut hcode[..HUF_ENCSIZE] {
        let l = *h;
        if l > 0 {
            *h = l | (n[l as usize] << 6);
            n[l as usize] += 1;
        }
    }
}

/// Port of `FHeapCompare` (src/lib/OpenEXRCore/internal_huf.c:208-212 @ v3.5.3): by frequency,
/// then by position in the table (upstream compares the pointers), so that the heap is the same
/// on every platform.
#[inline]
fn f_heap_compare(frq: &[u64], a: usize, b: usize) -> bool {
    (frq[a] > frq[b]) || ((frq[a] == frq[b]) && (a > b))
}

/// Port of `intern_push_heap` (src/lib/OpenEXRCore/internal_huf.c:214-226 @ v3.5.3).
fn intern_push_heap(
    frq: &[u64],
    first: &mut [usize],
    mut hole_index: usize,
    top_index: usize,
    value: usize,
) {
    let mut parent = hole_index.wrapping_sub(1) / 2;
    while hole_index > top_index && f_heap_compare(frq, first[parent], value) {
        first[hole_index] = first[parent];
        hole_index = parent;
        parent = hole_index.wrapping_sub(1) / 2;
    }
    first[hole_index] = value;
}

/// Port of `adjust_heap` (src/lib/OpenEXRCore/internal_huf.c:228-251 @ v3.5.3).
fn adjust_heap(frq: &[u64], first: &mut [usize], mut hole_index: usize, len: usize, value: usize) {
    let top_index = hole_index;
    let mut second_child = hole_index;

    while second_child < (len - 1) / 2 {
        second_child = 2 * (second_child + 1);
        if f_heap_compare(frq, first[second_child], first[second_child - 1]) {
            second_child -= 1;
        }
        first[hole_index] = first[second_child];
        hole_index = second_child;
    }

    if (len & 1) == 0 && second_child == (len - 2) / 2 {
        second_child = 2 * (second_child + 1);
        first[hole_index] = first[second_child - 1];
        hole_index = second_child - 1;
    }

    intern_push_heap(frq, first, hole_index, top_index, value);
}

/// Port of `push_heap` (src/lib/OpenEXRCore/internal_huf.c:253-258 @ v3.5.3): `heap`'s last
/// element into the heap before it.
fn push_heap(frq: &[u64], heap: &mut [usize]) {
    let value = heap[heap.len() - 1];
    let hole = heap.len() - 1;
    intern_push_heap(frq, heap, hole, 0, value);
}

/// Port of `pop_heap` and `intern_pop_heap` (src/lib/OpenEXRCore/internal_huf.c:260-276
/// @ v3.5.3): the top of `heap` to its end, the rest a heap again.
fn pop_heap(frq: &[u64], heap: &mut [usize]) {
    if heap.len() > 1 {
        let last = heap.len() - 1;
        let value = heap[last];
        heap[last] = heap[0];
        adjust_heap(frq, &mut heap[..last], 0, last, value);
    }
}

/// Port of `make_heap` (src/lib/OpenEXRCore/internal_huf.c:278-293 @ v3.5.3).
fn make_heap(frq: &[u64], first: &mut [usize]) {
    let len = first.len();
    if len < 2 {
        return;
    }
    let mut parent = (len - 2) / 2;
    loop {
        let value = first[parent];
        adjust_heap(frq, first, parent, len, value);
        if parent == 0 {
            return;
        }
        parent -= 1;
    }
}

/// Port of `hufBuildEncTable` (src/lib/OpenEXRCore/internal_huf.c:295-459 @ v3.5.3): the
/// Huffman codes of the frequencies in `frq` (which are destroyed), stored back into `frq` as
/// (code, length) pairs, with the range of used symbols `[im, iM]`; symbol `iM`, one past the
/// last used, is the run-length pseudo-symbol.
fn huf_build_enc_table(frq: &mut [u64], im: &mut u32, i_m: &mut u32) {
    let mut hlink = vec![0u32; HUF_ENCSIZE];
    let mut f_heap = vec![0usize; HUF_ENCSIZE];
    let mut scode = vec![0u64; HUF_ENCSIZE];

    // Find the minimum and maximum indices of non-zero entries in frq, fill fHeap with them,
    // and initialize hlink such that hlink[i] == i.
    let mut nf = 0usize;
    *im = 0;
    while frq[*im as usize] == 0 {
        *im += 1;
    }
    for i in *im as usize..HUF_ENCSIZE {
        hlink[i] = i as u32;
        if frq[i] != 0 {
            f_heap[nf] = i;
            nf += 1;
            *i_m = i as u32;
        }
    }

    // Add a pseudo-symbol, with a frequency count of 1, to frq; adjust the fHeap and hlink
    // array accordingly. Function hufEncode() uses the pseudo-symbol for run-length encoding.
    *i_m += 1;
    frq[*i_m as usize] = 1;
    f_heap[nf] = *i_m as usize;
    nf += 1;

    // Build an array, scode, such that scode[i] contains the number of bits assigned to
    // symbol i, by merging the two least frequent nodes until one is left.
    make_heap(frq, &mut f_heap[..nf]);

    while nf > 1 {
        // Find the indices, mm and m, of the two smallest non-zero frq values in fHeap, add
        // the smallest frq to the second-smallest frq, and remove the smallest frq value
        // from fHeap.
        let mm = f_heap[0];
        pop_heap(frq, &mut f_heap[..nf]);
        nf -= 1;

        let m = f_heap[0];
        pop_heap(frq, &mut f_heap[..nf]);

        frq[m] = frq[m].wrapping_add(frq[mm]);
        push_heap(frq, &mut f_heap[..nf]);

        // Add a bit to all codes in the first list, and merge the two lists.
        let mut j = m;
        loop {
            scode[j] += 1;
            if hlink[j] as usize == j {
                hlink[j] = mm as u32;
                break;
            }
            j = hlink[j] as usize;
        }

        // Add a bit to all codes in the second list
        let mut j = mm;
        loop {
            scode[j] += 1;
            if hlink[j] as usize == j {
                break;
            }
            j = hlink[j] as usize;
        }
    }

    // Build a canonical Huffman code table, replacing the code lengths in scode with (code,
    // code length) pairs. Copy the code table from scode into frq.
    huf_canonical_code_table(&mut scode);
    frq[..HUF_ENCSIZE].copy_from_slice(&scode);
}

/// Port of `hufPackEncTable` (src/lib/OpenEXRCore/internal_huf.c:476-529 @ v3.5.3): the code
/// lengths of symbols `im` to `iM`, 6 bits each, runs of zeroes shortened, into `out` from
/// `*pos` (which is advanced).
fn huf_pack_enc_table(
    hcode: &[u64],
    mut im: u32,
    i_m: u32,
    out: &mut [u8],
    pos: &mut usize,
) -> Result<(), Error> {
    let mut b = BitOut {
        c: 0,
        lc: 0,
        out,
        pos: *pos,
    };

    while im <= i_m {
        let l = huf_length(hcode[im as usize]);

        if l == 0 {
            let mut zerun: u64 = 1;
            while (im < i_m) && (zerun < LONGEST_LONG_RUN) {
                if huf_length(hcode[im as usize + 1]) > 0 {
                    break;
                }
                im += 1;
                zerun += 1;
            }

            if zerun >= 2 {
                if zerun >= SHORTEST_LONG_RUN {
                    b.output_bits(6, LONG_ZEROCODE_RUN)?;
                    b.output_bits(8, zerun - SHORTEST_LONG_RUN)?;
                } else {
                    b.output_bits(6, SHORT_ZEROCODE_RUN + zerun - 2)?;
                }
                im += 1;
                continue;
            }
        }

        b.output_bits(6, l as u64)?;
        im += 1;
    }

    if b.lc > 0 {
        if b.pos >= b.out.len() {
            return Err(bare(ErrorCode::ArgumentOutOfRange));
        }
        b.out[b.pos] = (b.c << (8 - b.lc)) as u8;
        b.pos += 1;
    }

    *pos = b.pos;
    Ok(())
}

/// Port of `hufUnpackEncTable` (src/lib/OpenEXRCore/internal_huf.c:535-591 @ v3.5.3): the code
/// lengths [`huf_pack_enc_table`] wrote, from `*pos` of `input` with `*n_left` bytes left (both
/// advanced), then the canonical codes, into `hcode`.
fn huf_unpack_enc_table(
    input: &[u8],
    pos: &mut usize,
    n_left: &mut u64,
    mut im: u32,
    i_m: u32,
    hcode: &mut [u64],
) -> Result<(), Error> {
    let start = *pos;
    let mut p = *pos;
    let mut c = 0u64;
    let ni = *n_left;
    let mut lc = 0u32;

    hcode[..HUF_ENCSIZE].fill(0);
    while im <= i_m {
        let nr = (p - start) as u64;
        if lc < 6 && nr >= ni {
            return Err(bare(ErrorCode::OutOfMemory));
        }

        let l = get_bits(6, &mut c, &mut lc, input, &mut p); // code length
        hcode[im as usize] = l;

        if l == LONG_ZEROCODE_RUN {
            let nr = (p - start) as u64;
            if lc < 8 && nr >= ni {
                return Err(bare(ErrorCode::OutOfMemory));
            }

            let mut zerun = get_bits(8, &mut c, &mut lc, input, &mut p) + SHORTEST_LONG_RUN;

            if u64::from(im) + zerun > u64::from(i_m) + 1 {
                return Err(bare(ErrorCode::CorruptChunk));
            }

            while zerun > 0 {
                zerun -= 1;
                hcode[im as usize] = 0;
                im += 1;
            }
            im -= 1;
        } else if l >= SHORT_ZEROCODE_RUN {
            let mut zerun = l - SHORT_ZEROCODE_RUN + 2;

            if u64::from(im) + zerun > u64::from(i_m) + 1 {
                return Err(bare(ErrorCode::CorruptChunk));
            }

            while zerun > 0 {
                zerun -= 1;
                hcode[im as usize] = 0;
                im += 1;
            }
            im -= 1;
        }
        im += 1;
    }

    let nr = (p - start) as u64;
    *n_left -= nr;
    *pos = p;

    huf_canonical_code_table(hcode);
    Ok(())
}

/// Port of `hufBuildDecTable` (src/lib/OpenEXRCore/internal_huf.c:615-713 @ v3.5.3): the
/// decoding table of the codes `hcode[im..=iM]`: short codes (up to `HUF_DECBITS` bits) fill
/// every entry they prefix, long codes are listed under the entry of their first bits.
fn huf_build_dec_table(
    hcode: &[u64],
    mut im: u32,
    i_m: u32,
    hdecod: &mut [HufDec],
) -> Result<(), Error> {
    while im <= i_m {
        let c = huf_code(hcode[im as usize]);
        let l = huf_length(hcode[im as usize]);

        if c >> l != 0 {
            // Error: c is supposed to be an l-bit code, but c contains a value that is
            // greater than the largest l-bit number.
            return Err(bare(ErrorCode::CorruptChunk));
        }

        if l > HUF_DECBITS as i32 {
            // Long code: add a secondary entry
            let pl = &mut hdecod[(c >> (l - HUF_DECBITS as i32)) as usize];
            if pl.len != 0 {
                // Error: a short code has already been stored in table entry *pl.
                return Err(bare(ErrorCode::CorruptChunk));
            }
            pl.lit += 1;
            pl.p.get_or_insert_with(Vec::new).push(im);
        } else if l != 0 {
            // Short code: init all primary entries
            let first = (c << (HUF_DECBITS as i32 - l)) as usize;
            for pl in &mut hdecod[first..first + (1usize << (HUF_DECBITS as i32 - l))] {
                if pl.len != 0 || pl.p.is_some() {
                    // Error: a short code or a long code has already been stored in table
                    // entry *pl.
                    return Err(bare(ErrorCode::CorruptChunk));
                }
                pl.len = l;
                pl.lit = im;
            }
        }
        im += 1;
    }
    Ok(())
}

/// Port of `hufEncode` (src/lib/OpenEXRCore/internal_huf.c:796-851 @ v3.5.3): the codes of
/// `input` into `out`, runs of a symbol as the run code `hcode[rlc]` and a count; returns the
/// number of bits written.
fn huf_encode(hcode: &[u64], input: &[u16], rlc: u32, out: &mut [u8]) -> Result<u32, Error> {
    let mut b = BitOut {
        c: 0,
        lc: 0,
        out,
        pos: 0,
    };
    let mut s = u32::from(input[0]);
    let mut cs: i32 = 0;
    let run_code = hcode[rlc as usize];

    // Loop on input values
    for &ns in &input[1..] {
        let ns = u32::from(ns);
        // Count same values or send code
        if cs == 255 || s != ns {
            b.send_code(hcode[s as usize], cs, run_code)?;
            cs = 0;
            s = ns;
        } else {
            cs += 1;
        }
    }

    // Send remaining code
    b.send_code(hcode[s as usize], cs, run_code)?;

    if b.lc != 0 {
        if b.pos >= b.out.len() {
            return Err(bare(ErrorCode::ArgumentOutOfRange));
        }
        // (the partial byte is not counted in the position, only in the bit count)
        b.out[b.pos] = ((b.c << (8 - b.lc)) & 0xff) as u8;
    }

    let c = (b.pos as u64) * 8 + b.lc as u64;
    if c > u64::from(u32::MAX) {
        return Err(bare(ErrorCode::ArgumentOutOfRange));
    }
    Ok(c as u32)
}

/// `getChar` and the input of the classic decoder (src/lib/OpenEXRCore/internal_huf.c:863-865
/// @ v3.5.3).
struct BitIn<'i> {
    c: u64,
    lc: i32,
    input: &'i [u8],
    pos: usize,
    end: usize,
}

impl BitIn<'_> {
    #[inline]
    fn get_char(&mut self) {
        self.c = (self.c << 8) | u64::from(self.input[self.pos]);
        self.pos += 1;
        self.lc += 8;
    }
}

/// `getCode` (src/lib/OpenEXRCore/internal_huf.c:867-898 @ v3.5.3): symbol `po` into `out` at
/// `*o`, or, for the run code, the previous symbol repeated by the next 8 bits.
#[inline]
fn get_code(
    po: u32,
    rlc: u32,
    b: &mut BitIn<'_>,
    out: &mut [u16],
    o: &mut usize,
) -> Result<(), Error> {
    if po == rlc {
        if b.lc < 8 {
            if b.pos >= b.end {
                return Err(bare(ErrorCode::OutOfMemory));
            }
            b.get_char();
        }

        b.lc -= 8;

        let mut cs = (b.c >> b.lc) as u8;

        if *o + usize::from(cs) > out.len() {
            return Err(bare(ErrorCode::CorruptChunk));
        } else if *o == 0 {
            return Err(bare(ErrorCode::OutOfMemory));
        }

        let s = out[*o - 1];

        while cs > 0 {
            cs -= 1;
            out[*o] = s;
            *o += 1;
        }
    } else if *o < out.len() {
        out[*o] = po as u16;
        *o += 1;
    } else {
        return Err(bare(ErrorCode::CorruptChunk));
    }
    Ok(())
}

/// Port of `hufDecode` (src/lib/OpenEXRCore/internal_huf.c:902-1013 @ v3.5.3): `ni` bits of
/// `input` decoded with the codes `hcode` and the table `hdecod` into all of `out`.
fn huf_decode(
    hcode: &[u64],
    hdecod: &[HufDec],
    input: &[u8],
    ni: u64,
    rlc: u32,
    out: &mut [u16],
) -> Result<(), Error> {
    let mut b = BitIn {
        c: 0,
        lc: 0,
        input,
        pos: 0,
        end: ni.div_ceil(8) as usize, // input byte size
    };
    let mut o = 0usize;

    // Loop on input bytes
    while b.pos < b.end {
        b.get_char();

        // Access decoding table
        while b.lc >= HUF_DECBITS as i32 {
            let decoffset = (b.c >> (b.lc - HUF_DECBITS as i32)) & HUF_DECMASK;
            let pl = &hdecod[decoffset as usize];

            if pl.len != 0 {
                // Get short code
                if pl.len > b.lc {
                    return Err(bare(ErrorCode::CorruptChunk));
                }
                b.lc -= pl.len;
                get_code(pl.lit, rlc, &mut b, out, &mut o)?;
            } else {
                // wrong code
                let Some(decbuf) = pl.p.as_ref() else {
                    return Err(bare(ErrorCode::CorruptChunk));
                };

                // Search long code
                let mut found = false;
                for &sym in &decbuf[..pl.lit as usize] {
                    let l = huf_length(hcode[sym as usize]);

                    while b.lc < l && b.pos < b.end {
                        // get more bits
                        b.get_char();
                    }

                    if b.lc >= l
                        && huf_code(hcode[sym as usize])
                            == ((b.c >> (b.lc - l)) & ((1u64 << l) - 1))
                    {
                        // Found : get long code
                        b.lc -= l;
                        get_code(sym, rlc, &mut b, out, &mut o)?;
                        found = true;
                        break;
                    }
                }

                if !found {
                    return Err(bare(ErrorCode::CorruptChunk));
                }
            }
        }
    }

    // Get remaining (short) codes
    let i = 8u64.wrapping_sub(ni) & 7;
    b.c >>= i;
    b.lc -= i as i32;

    while b.lc > 0 {
        let decoffset = (b.c << (HUF_DECBITS as i32 - b.lc)) & HUF_DECMASK;
        let pl = &hdecod[decoffset as usize];

        if pl.len != 0 {
            if pl.len > b.lc {
                return Err(bare(ErrorCode::CorruptChunk));
            }
            b.lc -= pl.len;
            get_code(pl.lit, rlc, &mut b, out, &mut o)?;
        } else {
            return Err(bare(ErrorCode::CorruptChunk));
        }
    }

    if o != out.len() {
        return Err(bare(ErrorCode::OutOfMemory));
    }
    Ok(())
}

/// Port of `writeUInt` (src/lib/OpenEXRCore/internal_huf.c:1023-1030 @ v3.5.3).
fn write_uint(b: &mut [u8], i: u32) {
    b[..4].copy_from_slice(&i.to_le_bytes());
}

/// Port of `readUInt` (src/lib/OpenEXRCore/internal_huf.c:1032-1038 @ v3.5.3).
fn read_uint(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// `READ64` (src/lib/OpenEXRCore/internal_huf.c:1060-1081 @ v3.5.3): 8 bytes, big-endian.
#[inline]
fn read64(input: &[u8], pos: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&input[pos..pos + 8]);
    u64::from_be_bytes(b)
}

/// The print_error of the fast decoder: through the context, when there is one.
fn corrupt(ctxt: Option<&Context>, msg: &str) -> Error {
    match ctxt {
        Some(c) => c.print_error(ErrorCode::CorruptChunk, msg, &[]),
        None => bare(ErrorCode::CorruptChunk),
    }
}

impl FastHufDecoder {
    fn new() -> FastHufDecoder {
        FastHufDecoder {
            rle_symbol: 0,
            num_symbols: 0,
            min_code_length: 0,
            max_code_length: 0,
            // zeroed, as new ones, from the thread's pool (speed only: docs/baseline.md)
            id_to_symbol: Kept::zeroed(65536 + 1),
            lj_base: [0; MAX_CODE_LEN + 1 + 1],
            lj_offset: [0; MAX_CODE_LEN + 1],
            table_symbol: Kept::zeroed(1 << TABLE_LOOKUP_BITS),
            table_code_len: Kept::zeroed(1 << TABLE_LOOKUP_BITS),
            table_min: 0,
        }
    }

    /// Port of `FastHufDecoder_buildTables` (src/lib/OpenEXRCore/internal_huf.c:1132-1231
    /// @ v3.5.3): the left justified base and offset tables, and the acceleration table of the
    /// codes up to `TABLE_LOOKUP_BITS` long.
    fn build_tables(
        &mut self,
        ctxt: Option<&Context>,
        base: &[u64; MAX_CODE_LEN + 1],
        offset: &[u64; MAX_CODE_LEN + 1],
    ) -> Result<(), Error> {
        let mut min_idx = TABLE_LOOKUP_BITS as i32;

        // Build the 'left justified' base table, by shifting base left..
        for i in 0..=MAX_CODE_LEN {
            if base[i] != u64::MAX {
                // (a shift by 64 for length 0, which x86 takes modulo 64)
                self.lj_base[i] = base[i].wrapping_shl(64 - i as u32);
            } else {
                // Unused code length - insert dummy values
                self.lj_base[i] = u64::MAX;
            }
        }
        self.lj_base[MAX_CODE_LEN + 1] = 0; // sentinel for brute force lookup

        // Combine some terms into a big fat constant, the 'left justified' offset table.
        self.lj_offset[0] = offset[0].wrapping_sub(self.lj_base[0]);
        for i in 1..=MAX_CODE_LEN {
            self.lj_offset[i] = offset[i].wrapping_sub(self.lj_base[i] >> (64 - i));
        }

        // Build the acceleration tables for the lookups of short codes ( <= TABLE_LOOKUP_BITS
        // long)
        for i in 0..(1u64 << TABLE_LOOKUP_BITS) {
            let value = i << INDEX_BIT_SHIFT;

            self.table_symbol[i as usize] = 0xffff;
            self.table_code_len[i as usize] = 0;

            let mut code_len = i32::from(self.min_code_length);
            while code_len <= i32::from(self.max_code_length) {
                let cl = code_len as usize;
                if self.lj_base[cl] <= value {
                    self.table_code_len[i as usize] = code_len as u8;
                    let id = self.lj_offset[cl].wrapping_add(value >> (64 - cl));
                    if id < u64::from(self.num_symbols) {
                        self.table_symbol[i as usize] = self.id_to_symbol[id as usize] as i32;
                    } else {
                        return Err(corrupt(ctxt, "Huffman decode error (Overrun)"));
                    }
                    break;
                }
                code_len += 1;
            }
        }

        // Store the smallest value in the table that points to real data. This should be the
        // entry for the largest length that has valid data (in our case, non-dummy _ljBase)
        while min_idx > 0 && self.lj_base[min_idx as usize] == u64::MAX {
            min_idx -= 1;
        }

        if min_idx < 0 {
            // Error, no codes with lengths 0-TABLE_LOOKUP_BITS used. Set the min value such
            // that the table is never tested.
            self.table_min = u64::MAX;
        } else {
            self.table_min = self.lj_base[min_idx as usize];
        }
        Ok(())
    }

    /// Port of `fasthuf_initialize` (src/lib/OpenEXRCore/internal_huf.c:1327-1551 @ v3.5.3):
    /// the decoder for the code table at `*table` of `input` (advanced past it; `num_bytes`
    /// left), of symbols `min_symbol` to `max_symbol`.
    #[allow(clippy::too_many_arguments)]
    fn initialize(
        &mut self,
        ctxt: Option<&Context>,
        input: &[u8],
        table: &mut usize,
        num_bytes: u64,
        min_symbol: u32,
        max_symbol: u32,
        rle_symbol: i32,
    ) -> Result<(), Error> {
        // The 'base' table is the minimum code at each code length. base[i] is the smallest
        // code (numerically) of length i.
        let mut base = [u64::MAX; MAX_CODE_LEN + 1];
        // The 'offset' table is the position (in sorted order) of the first id of a given code
        // length. Array is indexed by code length, like base.
        let mut offset = [0u64; MAX_CODE_LEN + 1];
        // Count of how many codes at each length there are.
        let mut code_count = [0usize; MAX_CODE_LEN + 1];

        let mut curr_byte = *table;
        let mut curr_bits = 0u64;
        let mut curr_bit_count = 0i32;
        let top_byte = (*table as u64).saturating_add(num_bytes);

        let mut mapping = [0u64; MAX_CODE_LEN + 1];

        self.rle_symbol = rle_symbol;
        self.num_symbols = 0;
        self.min_code_length = 255;
        self.max_code_length = 0;

        let truncated = "Error decoding Huffman table (Truncated table data).";

        // Count the number of codes, the min/max code lengths, the number of codes with each
        // length, and record symbols with non-zero code length as we find them.
        let mut symbol = u64::from(min_symbol);
        while symbol <= u64::from(max_symbol) {
            if curr_byte as u64 >= top_byte {
                return Err(corrupt(ctxt, truncated));
            }

            // Next code length - either:
            //       0-58  (literal code length)
            //       59-62 (various lengths runs of 0)
            //       63    (run of n 0's, with n is the next 8 bits)
            let code_len = fasthuf_read_bits(
                6,
                &mut curr_bits,
                &mut curr_bit_count,
                input,
                &mut curr_byte,
            );

            if code_len < SHORT_ZEROCODE_RUN {
                if code_len == 0 {
                    symbol += 1;
                    continue;
                }
                if code_len < u64::from(self.min_code_length) {
                    self.min_code_length = code_len as u8;
                }
                if code_len > u64::from(self.max_code_length) {
                    self.max_code_length = code_len as u8;
                }
                code_count[code_len as usize] += 1;
            } else if code_len == LONG_ZEROCODE_RUN {
                if curr_byte as u64 >= top_byte {
                    return Err(corrupt(ctxt, truncated));
                }
                symbol += fasthuf_read_bits(
                    8,
                    &mut curr_bits,
                    &mut curr_bit_count,
                    input,
                    &mut curr_byte,
                ) + SHORTEST_LONG_RUN
                    - 1;
            } else {
                symbol += code_len - SHORT_ZEROCODE_RUN + 1;
            }

            if symbol > u64::from(max_symbol) {
                return Err(corrupt(
                    ctxt,
                    "Error decoding Huffman table (Run beyond end of table).",
                ));
            }
            symbol += 1;
        }

        // (upstream leaves the codes of length MAX_CODE_LEN out of this count, which only
        // inputs of more than 2^58 samples could produce)
        for &n in &code_count[..MAX_CODE_LEN] {
            self.num_symbols = self.num_symbols.wrapping_add(n as u32);
        }

        if self.num_symbols as usize > self.id_to_symbol.len() {
            return Err(corrupt(
                ctxt,
                "Error decoding Huffman table (Too many symbols).",
            ));
        }

        // Compute base - once we have the code length counts, there is a closed form solution
        // for this (upstream uses the offset table as temporary space for the doubles; every
        // entry it uses is set again below)
        let (minl, maxl) = (
            usize::from(self.min_code_length),
            usize::from(self.max_code_length),
        );
        {
            let mut count_tmp = [0f64; MAX_CODE_LEN + 1];
            for l in minl..=maxl {
                count_tmp[l] = code_count[l] as f64 * (2i64 << (maxl - l)) as f64;
            }
            for l in minl..=maxl {
                let mut tmp = 0f64;
                for &t in &count_tmp[l + 1..=maxl] {
                    tmp += t;
                }
                tmp /= (2i64 << (maxl - l)) as f64;
                base[l] = tmp.ceil() as u64;
            }
        }

        // Compute offset - these are the positions of the first id (not symbol) that has
        // length [i]
        offset[maxl] = 0;
        let mut i = maxl as i32 - 1;
        while i >= minl as i32 {
            let iu = i as usize;
            offset[iu] = offset[iu + 1] + code_count[iu + 1] as u64;
            i -= 1;
        }

        // Allocate and fill the symbol-to-id mapping. Smaller Ids should be mapped to
        // less-frequent symbols (which have longer codes). Use the offset table to tell us
        // where the id's for a given code length start off.
        mapping.fill(u64::MAX);
        // (a loop, not a slice copy: with no codes the range is empty, min 255 and max 0)
        #[allow(clippy::manual_memcpy)]
        for i in minl..=maxl {
            mapping[i] = offset[i];
        }

        curr_byte = *table;
        curr_bits = 0;
        curr_bit_count = 0;

        // Although we could have created an uncompressed list of symbols in our decoding loop
        // above, it's faster to decode the compressed data again
        let mut symbol = u64::from(min_symbol);
        while symbol <= u64::from(max_symbol) {
            let code_len = fasthuf_read_bits(
                6,
                &mut curr_bits,
                &mut curr_bit_count,
                input,
                &mut curr_byte,
            );

            if code_len < SHORT_ZEROCODE_RUN {
                if code_len == 0 {
                    symbol += 1;
                    continue;
                }
                let m = &mut mapping[code_len as usize];
                if *m >= u64::from(self.num_symbols) {
                    return Err(corrupt(
                        ctxt,
                        "Huffman decode error (Invalid symbol in header)",
                    ));
                }
                self.id_to_symbol[*m as usize] = symbol as u32;
                *m += 1;
            } else if code_len == LONG_ZEROCODE_RUN {
                symbol += fasthuf_read_bits(
                    8,
                    &mut curr_bits,
                    &mut curr_bit_count,
                    input,
                    &mut curr_byte,
                ) + SHORTEST_LONG_RUN
                    - 1;
            } else {
                symbol += code_len - SHORT_ZEROCODE_RUN + 1;
            }
            symbol += 1;
        }

        *table = curr_byte;

        self.build_tables(ctxt, &base, &offset)
    }

    /// Port of `fasthuf_decode` (src/lib/OpenEXRCore/internal_huf.c:1597-1819 @ v3.5.3):
    /// `num_src_bits` bits of `input` from `src` decoded into all of `dst`.
    fn decode(
        &self,
        ctxt: Option<&Context>,
        input: &[u8],
        src: usize,
        mut num_src_bits: u64,
        dst: &mut [u16],
    ) -> Result<(), Error> {
        let num_dst_elems = dst.len() as u64;
        // Current position (byte/bit) in the src data stream (after the first buffer fill)
        let mut curr_byte = src + 2 * 8;

        num_src_bits -= 8 * 2 * 8;

        // 64-bit buffer holding the current bits in the stream
        let mut buffer = read64(input, src);
        let mut buffer_num_bits: i32 = 64;

        // 64-bit buffer holding the next bits in the stream
        let mut buffer_back = read64(input, src + 8);
        let mut buffer_back_num_bits: i32 = 64;
        let mut dst_idx: u64 = 0;

        let t_min = self.table_min;
        let rle_sym = self.rle_symbol as u32;

        while dst_idx < num_dst_elems {
            let symbol: i32;
            let code_len: i32;

            // Test if we can be table accelerated. If so, directly lookup the output symbol.
            // Otherwise, we need to fall back to searching for the code.
            if t_min <= buffer {
                let table_idx = (buffer >> INDEX_BIT_SHIFT) as usize;
                code_len = i32::from(self.table_code_len[table_idx]);
                symbol = self.table_symbol[table_idx];
            } else {
                if buffer_num_bits < 64 {
                    fast_huf_decoder_refill(
                        &mut buffer,
                        buffer_num_bits,
                        &mut buffer_back,
                        &mut buffer_back_num_bits,
                        input,
                        &mut curr_byte,
                        &mut num_src_bits,
                    );
                    buffer_num_bits = 64;
                }

                // Brute force search: find the smallest length where _ljBase[length] <= buffer
                let mut cl = TABLE_LOOKUP_BITS + 1;
                // sentinel zero can never be greater than buffer
                while self.lj_base[cl] > buffer {
                    cl += 1;
                }

                if cl > usize::from(self.max_code_length) {
                    return Err(corrupt(
                        ctxt,
                        "Huffman decode error (Decoded an invalid symbol)",
                    ));
                }

                let id = self.lj_offset[cl].wrapping_add(buffer >> (64 - cl));
                if id < u64::from(self.num_symbols) {
                    symbol = self.id_to_symbol[id as usize] as i32;
                } else {
                    return Err(corrupt(
                        ctxt,
                        "Huffman decode error (Decoded an invalid symbol)",
                    ));
                }
                code_len = cl as i32;
            }

            // Shift over bit stream, and update the bit count in the buffer
            buffer = buffer.wrapping_shl(code_len as u32);
            buffer_num_bits -= code_len;

            // If we received a RLE symbol (_rleSymbol), then we need to read ahead 8 bits to
            // know how many times to repeat the previous symbol. Need to ensure we at least
            // have 8 bits of data in the buffer
            if symbol == rle_sym as i32 {
                if buffer_num_bits < 8 {
                    fast_huf_decoder_refill(
                        &mut buffer,
                        buffer_num_bits,
                        &mut buffer_back,
                        &mut buffer_back_num_bits,
                        input,
                        &mut curr_byte,
                        &mut num_src_bits,
                    );
                    buffer_num_bits = 64;
                }

                let rle_count = (buffer >> 56) as u32;

                if dst_idx < 1 {
                    return Err(corrupt(
                        ctxt,
                        "Huffman decode error (RLE code with no previous symbol)",
                    ));
                }

                if dst_idx + u64::from(rle_count) > num_dst_elems {
                    return Err(corrupt(
                        ctxt,
                        "Huffman decode error (Symbol run beyond expected output buffer length)",
                    ));
                }

                if rle_count == 0 || rle_count >= i32::MAX as u32 {
                    return Err(corrupt(ctxt, "Huffman decode error (Invalid RLE length)"));
                }

                let prev = dst[(dst_idx - 1) as usize];
                for i in 0..u64::from(rle_count) {
                    dst[(dst_idx + i) as usize] = prev;
                }

                dst_idx += u64::from(rle_count);

                buffer <<= 8;
                buffer_num_bits -= 8;
            } else {
                dst[dst_idx as usize] = symbol as u16;
                dst_idx += 1;
            }

            // refill bit stream buffer if we're below the number of bits needed for a table
            // lookup
            if buffer_num_bits < TABLE_LOOKUP_BITS as i32 {
                fast_huf_decoder_refill(
                    &mut buffer,
                    buffer_num_bits,
                    &mut buffer_back,
                    &mut buffer_back_num_bits,
                    input,
                    &mut curr_byte,
                    &mut num_src_bits,
                );
                buffer_num_bits = 64;
            }
        }

        if num_src_bits != 0 {
            return Err(match ctxt {
                Some(c) => c.print_error(
                    ErrorCode::CorruptChunk,
                    "Huffman decode error (%d bits of compressed data remains after filling expected output buffer)",
                    &[Arg::from(num_src_bits as i32)],
                ),
                None => bare(ErrorCode::CorruptChunk),
            });
        }
        Ok(())
    }
}

/// Port of `FastHufDecoder_refill` (src/lib/OpenEXRCore/internal_huf.c:1233-1311 @ v3.5.3):
/// tops `buffer` (holding `num_bits`) up to 64 bits from `buffer_back`, refilling that from the
/// stream at `*curr_byte` (`*curr_bits_left` bits left).
#[allow(clippy::too_many_arguments)]
#[inline]
fn fast_huf_decoder_refill(
    buffer: &mut u64,
    mut num_bits: i32,
    buffer_back: &mut u64,
    buffer_back_num_bits: &mut i32,
    input: &[u8],
    curr_byte: &mut usize,
    curr_bits_left: &mut u64,
) {
    loop {
        // shifting right by 64 on a uint64_t is a bad time...
        if num_bits > 0 {
            let need_bits = 64 - num_bits;
            *buffer |= *buffer_back >> num_bits;
            if *buffer_back_num_bits >= need_bits {
                *buffer_back = buffer_back.wrapping_shl(need_bits as u32);
                *buffer_back_num_bits -= need_bits;
                break;
            }
            num_bits += *buffer_back_num_bits;
            *buffer_back_num_bits = 0;
        } else {
            *buffer = *buffer_back;
            num_bits = *buffer_back_num_bits;
            *buffer_back_num_bits = 0;
        }

        if *buffer_back_num_bits <= 0 {
            if *curr_bits_left >= 64 {
                *buffer_back = read64(input, *curr_byte);
                *buffer_back_num_bits = 64;
                *curr_byte += 8;
                *curr_bits_left -= 8 * 8;
            } else {
                let mut shift: i32 = 56;

                *buffer_back = 0;
                *buffer_back_num_bits = 64;

                while *curr_bits_left >= 8 {
                    *buffer_back |= u64::from(input[*curr_byte]) << shift;
                    *curr_byte += 1;
                    shift -= 8;
                    *curr_bits_left -= 8;
                }

                if *curr_bits_left > 0 {
                    *buffer_back |= u64::from(input[*curr_byte]) << shift;
                    *curr_byte += 1;
                    *curr_bits_left = 0;
                }
            }
        }
        if num_bits >= 64 {
            break;
        }
    }
}

/// Port of `fasthuf_read_bits` (src/lib/OpenEXRCore/internal_huf.c:1313-1325 @ v3.5.3).
#[inline]
fn fasthuf_read_bits(
    num_bits: i32,
    buffer: &mut u64,
    buffer_num_bits: &mut i32,
    input: &[u8],
    curr_byte: &mut usize,
) -> u64 {
    while *buffer_num_bits < num_bits {
        *buffer = (*buffer << 8) | u64::from(input[*curr_byte]);
        *curr_byte += 1;
        *buffer_num_bits += 8;
    }
    *buffer_num_bits -= num_bits;
    (*buffer >> *buffer_num_bits) & ((1u64 << num_bits) - 1)
}

/// Port of `fasthuf_decode_enabled` (src/lib/OpenEXRCore/internal_huf.c:1553-1595 @ v3.5.3):
/// enabled for GCC on x86-64 and for MSVC on x64, the builds of both wheels.
#[inline]
fn fasthuf_decode_enabled() -> bool {
    cfg!(target_arch = "x86_64")
}

/// Port of `internal_huf_compress` (src/lib/OpenEXRCore/internal_huf.c:1846-1914 @ v3.5.3): the
/// Huffman coding of `raw` into `out` (a 20-byte header: the symbol range, the table's length
/// and the data's bits; then the code table and the data); returns the bytes written.
pub(crate) fn internal_huf_compress(
    out: &mut [u8],
    raw: &[u16],
    sparebytes: u64,
) -> Result<u64, Error> {
    if raw.is_empty() {
        return Ok(0);
    }

    if out.len() < 20 {
        return Err(bare(ErrorCode::InvalidArgument));
    }
    // The spare buffer is partitioned using fixed offsets, so it only needs to be *at least*
    // the required size.
    if sparebytes < internal_exr_huf_compress_spare_bytes() {
        return Err(bare(ErrorCode::InvalidArgument));
    }

    // countFrequencies (src/lib/OpenEXRCore/internal_huf.c:1015-1021 @ v3.5.3)
    let mut freq = vec![0u64; HUF_ENCSIZE];
    for &d in raw {
        freq[d as usize] += 1;
    }

    let (mut im, mut i_m) = (0u32, 0u32);
    huf_build_enc_table(&mut freq, &mut im, &mut i_m);

    let mut table_end = 20usize;
    huf_pack_enc_table(&freq, im, i_m, out, &mut table_end)?;
    let table_length = (table_end - 20) as u32;
    let data_start = table_end;

    let n_bits = huf_encode(&freq, raw, i_m, &mut out[data_start..])?;

    let data_length = n_bits.div_ceil(8);

    write_uint(&mut out[0..], im);
    write_uint(&mut out[4..], i_m);
    write_uint(&mut out[8..], table_length);
    write_uint(&mut out[12..], n_bits);
    write_uint(&mut out[16..], 0); // room for future extensions

    Ok(data_start as u64 + u64::from(data_length))
}

/// Port of `internal_huf_decompress` (src/lib/OpenEXRCore/internal_huf.c:1916-2000 @ v3.5.3): the
/// Huffman data `compressed` decoded into all of `raw`, with the fast decoder for more than 128
/// bits and the classic one otherwise. `ctxt` is the decode pipeline's context, which the fast
/// decoder reports its errors through.
pub(crate) fn internal_huf_decompress(
    ctxt: Option<&Context>,
    compressed: &[u8],
    raw: &mut [u16],
    sparebytes: u64,
) -> Result<(), Error> {
    let n_compressed = compressed.len() as u64;
    let huf_info_block_size: u64 = 5 * 4;

    // need at least 20 bytes for header
    if n_compressed < 20 {
        if !raw.is_empty() {
            return Err(bare(ErrorCode::InvalidArgument));
        }
        return Ok(());
    }

    // The spare buffer is partitioned using fixed offsets, so it only needs to be *at least*
    // the required size.
    if sparebytes < internal_exr_huf_decompress_spare_bytes() {
        return Err(bare(ErrorCode::InvalidArgument));
    }

    let im = read_uint(compressed);
    let i_m = read_uint(&compressed[4..]);
    // uint32_t tableLength = readUInt (compressed + 8);
    let n_bits = read_uint(&compressed[12..]);
    // uint32_t future = readUInt (compressed + 16);

    if im as usize >= HUF_ENCSIZE || i_m as usize >= HUF_ENCSIZE {
        return Err(bare(ErrorCode::CorruptChunk));
    }

    let mut ptr = huf_info_block_size as usize;

    let n_bytes = (u64::from(n_bits) + 7) / 8;

    // must be nBytes remaining in buffer
    if huf_info_block_size + n_bytes > n_compressed {
        return Err(bare(ErrorCode::OutOfMemory));
    }

    // Fast decoder needs at least 2x64-bits of compressed data, and needs to be run-able on
    // this platform. Otherwise, fall back to the original decoder
    if fasthuf_decode_enabled() && n_bits > 128 {
        let mut fhd = FastHufDecoder::new();
        fhd.initialize(
            ctxt,
            compressed,
            &mut ptr,
            n_compressed - huf_info_block_size,
            im,
            i_m,
            i_m as i32,
        )?;
        if ptr as u64 + n_bytes > n_compressed {
            return Err(bare(ErrorCode::OutOfMemory));
        }
        fhd.decode(ctxt, compressed, ptr, u64::from(n_bits), raw)
    } else {
        let mut freq = vec![0u64; HUF_ENCSIZE];
        let mut hdec = vec![HufDec::default(); HUF_DECSIZE];
        let mut n_left = n_compressed - 20;

        // (upstream does not check the table's unpacking; a corrupt table is caught by the
        // checks that follow)
        let _ = huf_unpack_enc_table(compressed, &mut ptr, &mut n_left, im, i_m, &mut freq);

        if u64::from(n_bits) > 8 * n_left {
            return Err(bare(ErrorCode::CorruptChunk));
        }

        huf_build_dec_table(&freq, im, i_m, &mut hdec)?;
        huf_decode(
            &freq,
            &hdec,
            &compressed[ptr..],
            u64::from(n_bits),
            i_m,
            raw,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct sizes are C's on x86-64: `HufDec` is two 4-byte fields and a pointer, and
    /// `FastHufDecoder`'s fields add up, 8-byte aligned, with no padding but its own.
    #[test]
    fn layouts_are_cs() {
        assert_eq!(size_of::<HufDecLayout>(), 4 + 4 + 8);
        let fast = 4 + 4 + 4 + 4 * 65537 + 8 * 60 + 8 * 59 + 4 * 4096 + 4096;
        assert_eq!(size_of::<FastHufDecoderLayout>(), fast + 8);
    }

    /// Coded data decodes to its input, with the classic and the fast decoder.
    #[test]
    fn round_trips() {
        let mut rng = openexr_testkit::probe::Rng::new(7);
        let mut inputs: Vec<Vec<u16>> = vec![vec![5], vec![1, 1, 1, 1], vec![0; 3000]];
        inputs.push((0..20000).map(|_| (rng.next_u64() % 7) as u16).collect());
        inputs.push((0..20000).map(|_| rng.next_u64() as u16).collect());
        inputs.push((0..5000).map(|i| (i / 300) as u16).collect());
        for input in inputs {
            let mut out = vec![0u8; input.len() * 4 + 65536 * 2];
            let n = internal_huf_compress(&mut out, &input, u64::MAX).unwrap() as usize;
            let mut back = vec![0u16; input.len()];
            internal_huf_decompress(None, &out[..n], &mut back, u64::MAX).unwrap();
            assert_eq!(back, input, "{} samples", input.len());
        }
    }
}
