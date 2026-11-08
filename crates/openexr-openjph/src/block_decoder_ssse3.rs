// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_decoder_ssse3.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:1-2070 @ OpenJPH 0.32.0):
//! the HT block decoder upstream chooses from `X86_CPU_EXT_LEVEL_SSSE3` below AVX2
//! (`codeblock_fun::init`; built with `-mssse3`), in SSSE3. Its MEL and backward (VLC and
//! MagRef) readers are the generic decoder's ([`DecMel`], [`Rev`]: upstream's are the same
//! code); its forward reader (`frwd_struct_ssse3`, MagSgn and SPP) is its own, 16 bytes a read.
//!
//! Memory: upstream's forward reader loads 16 bytes wherever its segment ends, keeping only
//! the segment's (the rest it replaces by the fill byte); here only the segment's bytes are
//! read, the others being replaced alike. Its stores past the codeblock's width and height
//! land in the codeblock buffer's padding, which [`ojph_decode_codeblock_ssse3`] checks once.

#![allow(unsafe_code)]

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::arch::count_leading_zeros;
use crate::block_common::tables;
use crate::block_decoder32::{DecMel, Rev};
use crate::message::{Arg, warn};

const FILE: &str = "ojph_block_decoder_ssse3.cpp";

/// Port of `frwd_struct_ssse3`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:584-590 @ OpenJPH 0.32.0).
struct Frwd<'a> {
    buf: &'a [u8],
    /// The read position in `buf`.
    data: usize,
    /// read data, 16 extra bytes
    tmp: [u8; 48],
    /// number of bits stored in tmp
    bits: u32,
    /// 1 if a bit needs to be unstuffed from next byte
    unstuff: u32,
    /// size of data
    size: i32,
}

impl<'a> Frwd<'a> {
    /// Port of `frwd_read`
    /// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:610-688 @ OpenJPH 0.32.0):
    /// 16 more bytes, unstuffed, the bytes past the segment `X`.
    #[target_feature(enable = "ssse3")]
    fn read<const X: u32>(&mut self) {
        // (upstream loads 16 bytes at data; only the first `bytes` are kept)
        let bytes = if self.size >= 16 { 16 } else { self.size };
        let mut raw = [0u8; 16];
        let keep = (bytes.max(0) as usize).min(self.buf.len().saturating_sub(self.data));
        raw[..keep].copy_from_slice(&self.buf[self.data..self.data + keep]);
        // SAFETY: raw holds 16 bytes
        let mut val = unsafe { _mm_loadu_si128(raw.as_ptr() as *const __m128i) };
        let mut validity = _mm_set1_epi8(bytes as i8);
        self.data += bytes.max(0) as usize;
        self.size -= bytes;
        let mut bits: i32 = 128;
        let offset = _mm_set_epi64x(0x0F0E0D0C0B0A0908, 0x0706050403020100);
        validity = _mm_cmpgt_epi8(validity, offset);
        let all_xff = _mm_set1_epi8(-1);
        if X == 0xFF {
            let t = _mm_xor_si128(validity, all_xff); // complement
            val = _mm_or_si128(t, val); // fill with 0xFF
        } else {
            val = _mm_and_si128(validity, val); // fill with zeros
        }

        let mut ff_bytes = _mm_cmpeq_epi8(val, all_xff);
        ff_bytes = _mm_and_si128(ff_bytes, validity);
        let mut flags = _mm_movemask_epi8(ff_bytes) as u32;
        flags <<= 1; // unstuff following byte
        let next_unstuff = flags >> 16;
        flags |= self.unstuff;
        flags &= 0xFFFF;
        while flags != 0 {
            // bit unstuffing occurs on average once every 256 bytes
            bits -= 1; // consuming one stuffing bit

            let loc = 31 - count_leading_zeros(flags);
            flags ^= 1 << loc;

            let mut t = _mm_set1_epi8(loc as i8);
            let m = _mm_cmpgt_epi8(offset, t);

            t = _mm_and_si128(m, val); // keep bits at locations larger than loc
            let c = _mm_srli_epi64::<1>(t); // 1 bits left
            t = _mm_srli_si128::<8>(t); // 8 bytes left
            t = _mm_slli_epi64::<63>(t); // keep the MSB only
            t = _mm_or_si128(t, c); // combine the above 3 steps

            val = _mm_or_si128(t, _mm_andnot_si128(m, val));
        }

        // combine with earlier data
        let cur_bytes = (self.bits >> 3) as usize;
        let cur_bits = (self.bits & 7) as i64;
        let mut b1 = _mm_sll_epi64(val, _mm_set1_epi64x(cur_bits));
        let mut b2 = _mm_slli_si128::<8>(val); // 8 bytes right
        b2 = _mm_srl_epi64(b2, _mm_set1_epi64x(64 - cur_bits));
        b1 = _mm_or_si128(b1, b2);
        // SAFETY: cur_bytes <= 16 (bits <= 128), inside tmp's 48 bytes
        unsafe {
            b2 = _mm_loadu_si128(self.tmp.as_ptr().add(cur_bytes) as *const __m128i);
            b2 = _mm_or_si128(b1, b2);
            _mm_storeu_si128(self.tmp.as_mut_ptr().add(cur_bytes) as *mut __m128i, b2);
        }

        let room = 128 - cur_bits as i32;
        let consumed_bits = if bits < room { bits } else { room };
        let cur_bytes = ((self.bits + consumed_bits as u32 + 7) >> 3) as usize; // round up
        let mut upper = _mm_extract_epi16::<7>(val);
        upper >>= consumed_bits - 128 + 16;
        self.tmp[cur_bytes] = upper as u8; // copy byte

        self.bits += bits as u32;
        self.unstuff = next_unstuff; // next unstuff
    }

    /// Port of `frwd_init`
    /// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:699-713 @ OpenJPH 0.32.0):
    /// the segment of `size` bytes at `start` of `buf`.
    #[target_feature(enable = "ssse3")]
    fn init<const X: u32>(buf: &'a [u8], start: usize, size: i32) -> Self {
        let mut f = Frwd {
            buf,
            data: start,
            tmp: [0; 48],
            bits: 0,
            unstuff: 0,
            size,
        };
        f.read::<X>(); // read 128 bits more
        f
    }

    /// Port of `frwd_advance`
    /// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:721-751 @ OpenJPH 0.32.0).
    #[target_feature(enable = "ssse3")]
    fn advance(&mut self, num_bits: u32) {
        self.bits = self.bits.wrapping_sub(num_bits);

        let at = ((num_bits >> 3) & 0x18) as usize;
        let n = i64::from(num_bits & 63);

        // SAFETY: at <= 8 (num_bits < 128), so the two loads end within tmp's 48 bytes
        let (v0, v1) = unsafe {
            let p = self.tmp.as_ptr().add(at);
            (
                _mm_loadu_si128(p as *const __m128i),
                _mm_loadu_si128(p.add(16) as *const __m128i),
            )
        };

        // shift right by num_bits
        let mut c0 = _mm_srl_epi64(v0, _mm_set1_epi64x(n));
        let mut t = _mm_srli_si128::<8>(v0);
        t = _mm_sll_epi64(t, _mm_set1_epi64x(64 - n));
        c0 = _mm_or_si128(c0, t);
        t = _mm_slli_si128::<8>(v1);
        t = _mm_sll_epi64(t, _mm_set1_epi64x(64 - n));
        c0 = _mm_or_si128(c0, t);

        let mut c1 = _mm_srl_epi64(v1, _mm_set1_epi64x(n));
        t = _mm_srli_si128::<8>(v1);
        t = _mm_sll_epi64(t, _mm_set1_epi64x(64 - n));
        c1 = _mm_or_si128(c1, t);

        // SAFETY: tmp holds 48 bytes
        unsafe {
            _mm_storeu_si128(self.tmp.as_mut_ptr() as *mut __m128i, c0);
            _mm_storeu_si128(self.tmp.as_mut_ptr().add(16) as *mut __m128i, c1);
        }
    }

    /// Port of `frwd_fetch`
    /// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:760-772 @ OpenJPH 0.32.0).
    #[target_feature(enable = "ssse3")]
    fn fetch<const X: u32>(&mut self) -> __m128i {
        if self.bits <= 128 {
            self.read::<X>();
            if self.bits <= 128 {
                // need to test
                self.read::<X>();
            }
        }
        // SAFETY: tmp holds 48 bytes
        unsafe { _mm_loadu_si128(self.tmp.as_ptr() as *const __m128i) }
    }
}

/// Port of `decode_one_quad32`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:786-885 @ OpenJPH 0.32.0):
/// quad `N` (0 or 1) of an octet.
#[target_feature(enable = "ssse3")]
fn decode_one_quad32<const N: i32>(
    inf_u_q: __m128i,
    u_q: __m128i,
    magsgn: &mut Frwd<'_>,
    p: u32,
    vn: &mut __m128i,
) -> __m128i {
    let mut row = _mm_setzero_si128();
    let mut w0 = if N == 0 {
        _mm_shuffle_epi32::<0x00>(inf_u_q)
    } else {
        _mm_shuffle_epi32::<0x55>(inf_u_q)
    };
    // we keeps e_k, e_1, and rho in w2
    let mut flags = _mm_and_si128(w0, _mm_set_epi32(0x8880, 0x4440, 0x2220, 0x1110));
    let insig = _mm_cmpeq_epi32(flags, _mm_setzero_si128());
    if _mm_movemask_epi8(insig) != 0xFFFF {
        // are all insignificant?
        let u_q = if N == 0 {
            _mm_shuffle_epi32::<0x00>(u_q)
        } else {
            _mm_shuffle_epi32::<0x55>(u_q)
        };
        flags = _mm_mullo_epi16(flags, _mm_set_epi16(1, 1, 2, 2, 4, 4, 8, 8));
        let mut ms_vec = magsgn.fetch::<0xFF>();

        // next e_k and m_n
        w0 = _mm_srli_epi32::<15>(flags); // e_k
        let mut m_n = _mm_sub_epi32(u_q, w0);
        m_n = _mm_andnot_si128(insig, m_n);

        // find cumulative sums to find at which bit in ms_vec the sample starts
        let mut inc_sum = m_n; // inclusive scan
        inc_sum = _mm_add_epi32(inc_sum, _mm_bslli_si128::<4>(inc_sum));
        inc_sum = _mm_add_epi32(inc_sum, _mm_bslli_si128::<8>(inc_sum));
        let total_mn = _mm_extract_epi16::<6>(inc_sum);
        let ex_sum = _mm_bslli_si128::<4>(inc_sum); // exclusive scan

        // find the starting byte and starting bit
        let mut byte_idx = _mm_srli_epi32::<3>(ex_sum);
        let mut bit_idx = _mm_and_si128(ex_sum, _mm_set1_epi32(7));
        byte_idx = _mm_shuffle_epi8(
            byte_idx,
            _mm_set_epi32(0x0C0C0C0C, 0x08080808, 0x04040404, 0x00000000),
        );
        byte_idx = _mm_add_epi32(byte_idx, _mm_set1_epi32(0x03020100));
        let mut d0 = _mm_shuffle_epi8(ms_vec, byte_idx);
        byte_idx = _mm_add_epi32(byte_idx, _mm_set1_epi32(0x01010101));
        let mut d1 = _mm_shuffle_epi8(ms_vec, byte_idx);

        // shift samples values to correct location
        bit_idx = _mm_or_si128(bit_idx, _mm_slli_epi32::<16>(bit_idx));
        let mut bit_shift = _mm_shuffle_epi8(
            _mm_set_epi8(1, 3, 7, 15, 31, 63, 127, -1, 1, 3, 7, 15, 31, 63, 127, -1),
            bit_idx,
        );
        bit_shift = _mm_add_epi16(bit_shift, _mm_set1_epi16(0x0101));
        d0 = _mm_mullo_epi16(d0, bit_shift);
        d0 = _mm_srli_epi16::<8>(d0); // we should have 8 bits in the LSB
        d1 = _mm_mullo_epi16(d1, bit_shift);
        d1 = _mm_and_si128(d1, _mm_set1_epi32(0xFF00FF00_u32 as i32)); // 8 in MSB
        d0 = _mm_or_si128(d0, d1);

        // find location of e_k and mask
        let ones = _mm_set1_epi32(1);
        let twos = _mm_set1_epi32(2);
        let mut u_q_m1 = _mm_sub_epi32(u_q, ones);
        u_q_m1 = _mm_and_si128(u_q_m1, _mm_set_epi32(0, 0, 0, 0x1F));
        w0 = _mm_sub_epi32(twos, w0);
        let shift = _mm_sll_epi32(w0, u_q_m1); // U_q_m1 must be no more than 31
        ms_vec = _mm_and_si128(d0, _mm_sub_epi32(shift, ones));

        // next e_1
        w0 = _mm_and_si128(flags, _mm_set1_epi32(0x800));
        w0 = _mm_cmpeq_epi32(w0, _mm_setzero_si128());
        w0 = _mm_andnot_si128(w0, shift); // e_1 in correct position
        ms_vec = _mm_or_si128(ms_vec, w0); // e_1
        w0 = _mm_slli_epi32::<31>(ms_vec); // sign
        ms_vec = _mm_or_si128(ms_vec, ones); // bin center
        let mut tvn = ms_vec;
        ms_vec = _mm_add_epi32(ms_vec, twos); // + 2
        ms_vec = _mm_sll_epi32(ms_vec, _mm_cvtsi32_si128(p as i32 - 1));
        ms_vec = _mm_or_si128(ms_vec, w0); // sign
        row = _mm_andnot_si128(insig, ms_vec); // significant only

        ms_vec = _mm_andnot_si128(insig, tvn); // significant only
        tvn = if N == 0 {
            _mm_shuffle_epi8(ms_vec, _mm_set_epi32(-1, -1, 0x0F0E0D0C, 0x07060504))
        } else {
            _mm_shuffle_epi8(ms_vec, _mm_set_epi32(-1, 0x0F0E0D0C, 0x07060504, -1))
        };
        *vn = _mm_or_si128(*vn, tvn);

        if total_mn != 0 {
            magsgn.advance(total_mn as u32);
        }
    }
    row
}

/// Port of `decode_two_quad16`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:897-1004 @ OpenJPH 0.32.0).
#[target_feature(enable = "ssse3")]
fn decode_two_quad16(
    inf_u_q: __m128i,
    u_q: __m128i,
    magsgn: &mut Frwd<'_>,
    p: u32,
    vn: &mut __m128i,
) -> __m128i {
    let mut row = _mm_setzero_si128();
    let mut w0 = _mm_shuffle_epi8(
        inf_u_q,
        _mm_set_epi16(
            0x0504, 0x0504, 0x0504, 0x0504, 0x0100, 0x0100, 0x0100, 0x0100,
        ),
    );
    // we keeps e_k, e_1, and rho in w2
    let mut flags = _mm_and_si128(
        w0,
        _mm_set_epi16(
            0x8880_u16 as i16,
            0x4440,
            0x2220,
            0x1110,
            0x8880_u16 as i16,
            0x4440,
            0x2220,
            0x1110,
        ),
    );
    let insig = _mm_cmpeq_epi16(flags, _mm_setzero_si128());
    if _mm_movemask_epi8(insig) != 0xFFFF {
        // are all insignificant?
        let u_q = _mm_shuffle_epi8(
            u_q,
            _mm_set_epi16(
                0x0504, 0x0504, 0x0504, 0x0504, 0x0100, 0x0100, 0x0100, 0x0100,
            ),
        );
        flags = _mm_mullo_epi16(flags, _mm_set_epi16(1, 2, 4, 8, 1, 2, 4, 8));
        let mut ms_vec = magsgn.fetch::<0xFF>();

        // next e_k and m_n
        w0 = _mm_srli_epi16::<15>(flags); // e_k
        let mut m_n = _mm_sub_epi16(u_q, w0);
        m_n = _mm_andnot_si128(insig, m_n);

        // find cumulative sums to find at which bit in ms_vec the sample starts
        let mut inc_sum = m_n; // inclusive scan
        inc_sum = _mm_add_epi16(inc_sum, _mm_bslli_si128::<2>(inc_sum));
        inc_sum = _mm_add_epi16(inc_sum, _mm_bslli_si128::<4>(inc_sum));
        inc_sum = _mm_add_epi16(inc_sum, _mm_bslli_si128::<8>(inc_sum));
        let total_mn = _mm_extract_epi16::<7>(inc_sum);
        let ex_sum = _mm_bslli_si128::<2>(inc_sum); // exclusive scan

        // find the starting byte and starting bit
        let mut byte_idx = _mm_srli_epi16::<3>(ex_sum);
        let bit_idx = _mm_and_si128(ex_sum, _mm_set1_epi16(7));
        byte_idx = _mm_shuffle_epi8(
            byte_idx,
            _mm_set_epi16(
                0x0E0E, 0x0C0C, 0x0A0A, 0x0808, 0x0606, 0x0404, 0x0202, 0x0000,
            ),
        );
        byte_idx = _mm_add_epi16(byte_idx, _mm_set1_epi16(0x0100));
        let mut d0 = _mm_shuffle_epi8(ms_vec, byte_idx);
        byte_idx = _mm_add_epi16(byte_idx, _mm_set1_epi16(0x0101));
        let mut d1 = _mm_shuffle_epi8(ms_vec, byte_idx);

        // shift samples values to correct location
        let mut bit_shift = _mm_shuffle_epi8(
            _mm_set_epi8(1, 3, 7, 15, 31, 63, 127, -1, 1, 3, 7, 15, 31, 63, 127, -1),
            bit_idx,
        );
        bit_shift = _mm_add_epi16(bit_shift, _mm_set1_epi16(0x0101));
        d0 = _mm_mullo_epi16(d0, bit_shift);
        d0 = _mm_srli_epi16::<8>(d0); // we should have 8 bits in the LSB
        d1 = _mm_mullo_epi16(d1, bit_shift);
        d1 = _mm_and_si128(d1, _mm_set1_epi16(0xFF00_u16 as i16)); // 8 in MSB
        d0 = _mm_or_si128(d0, d1);

        // find location of e_k and mask
        let ones = _mm_set1_epi16(1);
        let twos = _mm_set1_epi16(2);
        let u_q_m1 = _mm_sub_epi32(u_q, ones);
        let uq0 = _mm_and_si128(u_q_m1, _mm_set_epi32(0, 0, 0, 0x1F));
        let uq1 = _mm_bsrli_si128::<14>(u_q_m1);
        w0 = _mm_sub_epi16(twos, w0);
        let mut t0 = _mm_and_si128(w0, _mm_set_epi64x(0, -1));
        let mut t1 = _mm_and_si128(w0, _mm_set_epi64x(-1, 0));
        t0 = _mm_sll_epi16(t0, uq0);
        t1 = _mm_sll_epi16(t1, uq1);
        let shift = _mm_or_si128(t0, t1);
        ms_vec = _mm_and_si128(d0, _mm_sub_epi16(shift, ones));

        // next e_1
        w0 = _mm_and_si128(flags, _mm_set1_epi16(0x800));
        w0 = _mm_cmpeq_epi16(w0, _mm_setzero_si128());
        w0 = _mm_andnot_si128(w0, shift); // e_1 in correct position
        ms_vec = _mm_or_si128(ms_vec, w0); // e_1
        w0 = _mm_slli_epi16::<15>(ms_vec); // sign
        ms_vec = _mm_or_si128(ms_vec, ones); // bin center
        let tvn = ms_vec;
        ms_vec = _mm_add_epi16(ms_vec, twos); // + 2
        ms_vec = _mm_sll_epi16(ms_vec, _mm_cvtsi32_si128(p as i32 - 1));
        ms_vec = _mm_or_si128(ms_vec, w0); // sign
        row = _mm_andnot_si128(insig, ms_vec); // significant only

        ms_vec = _mm_andnot_si128(insig, tvn); // significant only
        w0 = _mm_shuffle_epi8(
            ms_vec,
            _mm_set_epi16(-1, -1, -1, -1, -1, -1, 0x0706, 0x0302),
        );
        *vn = _mm_or_si128(*vn, w0);
        w0 = _mm_shuffle_epi8(
            ms_vec,
            _mm_set_epi16(-1, -1, -1, -1, -1, 0x0F0E, 0x0B0A, -1),
        );
        *vn = _mm_or_si128(*vn, w0);

        if total_mn != 0 {
            magsgn.advance(total_mn as u32);
        }
    }
    row
}

/// Spreads the 16 bits of `bits` into the 16 bytes of a vector, 0xFF where set.
#[target_feature(enable = "ssse3")]
#[inline]
fn spread16(bits: u32) -> __m128i {
    let mut v = _mm_set1_epi16(bits as u16 as i16);
    v = _mm_shuffle_epi8(
        v,
        _mm_set_epi8(1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0),
    );
    v = _mm_and_si128(v, _mm_set1_epi64x(0x8040201008040201_u64 as i64));
    _mm_cmpeq_epi8(v, _mm_set1_epi64x(0x8040201008040201_u64 as i64))
}

static INSUFFICIENT_PRECISION: AtomicBool = AtomicBool::new(false);
static MODIFY_CODE: AtomicBool = AtomicBool::new(false);
static TRUNCATE_SPP_MRP: AtomicBool = AtomicBool::new(false);

/// `v_n_size`.
const V_N_SIZE: usize = 512 + 8;
/// The scratch array's entries: upstream's `8 * 513`, and 16 more for its loads of 8 entries
/// (which end within upstream's array).
const SCRATCH: usize = 8 * 513 + 16;

/// Port of `ojph_decode_codeblock_ssse3`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_ssse3.cpp:1024-2066 @ OpenJPH 0.32.0):
/// decodes one codeblock, the cleanup, significance propagation and magnitude refinement
/// passes. `None` when the buffers cannot hold every access upstream makes.
///
/// # Safety
/// The CPU supports SSSE3.
#[target_feature(enable = "ssse3")]
#[allow(clippy::too_many_arguments)]
unsafe fn ojph_decode_codeblock_ssse3(
    coded_data: &[u8],
    decoded_data: &mut [u32],
    missing_msbs: u32,
    mut num_passes: u32,
    lengths1: u32,
    lengths2: u32,
    width: u32,
    height: u32,
    stride: u32,
    stripe_causal: bool,
) -> Option<bool> {
    // the accesses upstream makes, checked up front: rows of 4 below the height rounded up
    // to 4, 4 samples a store below the stride (a multiple of 4); the coded data's segments
    if width == 0
        || height == 0
        || width > 1024
        || height > 1024
        || width * height > 4096
        || stride % 4 != 0
        || stride < ((width + 3) & !3)
        || (((height + 3) & !3) as usize) * (stride as usize) > decoded_data.len()
        || (lengths1 as usize) + (lengths2 as usize) > coded_data.len()
    {
        return None;
    }

    if num_passes > 1 && lengths2 == 0 {
        warn(
            0x00010001,
            FILE,
            1036,
            "A malformed codeblock that has more than one coding pass, but zero length for 2nd and potential 3rd pass.",
            &[],
        );
        num_passes = 1;
    }

    if num_passes > 3 {
        warn(
            0x00010002,
            FILE,
            1044,
            "We do not support more than 3 coding passes; This codeblocks has %d passes.",
            &[Arg::Int(num_passes.into())],
        );
        return Some(false);
    }

    if missing_msbs > 30 {
        // p < 0
        if !INSUFFICIENT_PRECISION.swap(true, Ordering::Relaxed) {
            warn(
                0x00010003,
                FILE,
                1055,
                "32 bits are not enough to decode this codeblock. This message will not be displayed again.",
                &[],
            );
        }
        return Some(false);
    } else if missing_msbs == 30 {
        // p == 0
        if !MODIFY_CODE.swap(true, Ordering::Relaxed) {
            warn(
                0x00010004,
                FILE,
                1065,
                "Not enough precision to decode the cleanup pass. The code can be modified to support this case. This message will not be displayed again.",
                &[],
            );
        }
        return Some(false); // 32 bits are not enough to decode this
    } else if missing_msbs == 29 {
        // if p is 1, then num_passes must be 1
        if num_passes > 1 {
            num_passes = 1;
            if !TRUNCATE_SPP_MRP.swap(true, Ordering::Relaxed) {
                warn(
                    0x00010005,
                    FILE,
                    1078,
                    "Not enough precision to decode the SgnProp nor MagRef passes; both will be skipped. This message will not be displayed again.",
                    &[],
                );
            }
        }
    }
    let mut p = 30 - missing_msbs; // The least significant bitplane for CUP

    if lengths1 < 2 {
        warn(0x00010006, FILE, 1091, "Wrong codeblock length.", &[]);
        return Some(false);
    }

    // read scup and fix the bytes there
    let lcup = lengths1 as i32; // length of CUP
    // scup is the length of MEL + VLC
    let scup = (i32::from(coded_data[(lcup - 1) as usize]) << 4)
        + i32::from(coded_data[(lcup - 2) as usize] & 0xF);
    if scup < 2 || scup > lcup || scup > 4079 {
        // something is wrong
        return Some(false);
    }

    let mut scratch = [0u16; SCRATCH]; // 8+ kB

    // We need an extra two entries (one inf and one u_q) beyond the last column.
    let sstr = (((width + 2) + 7) & !7) as usize; // multiples of 8

    let mmsbp2 = missing_msbs + 2;
    let t = tables();
    let stride_u = stride as usize;

    // step 1 decoding VLC and MEL segments
    {
        // init structures
        let mut mel = DecMel::init(coded_data, lcup, scup);
        let mut vlc = Rev::init_vlc(coded_data, lcup, scup);

        let mut run = mel.get_run(); // decode runs of events from MEL bitstrm

        let mut vlc_val;
        let mut c_q: u32 = 0;
        let mut sp = 0usize;
        // initial quad row
        let mut x = 0u32;
        while x < width {
            // first quad
            vlc_val = vlc.fetch();

            // decode VLC using the context c_q and the head of VLC bitstream
            let mut t0 = t.vlc_tbl0[(c_q + (vlc_val & 0x7F)) as usize];

            // if context is zero, use one MEL event
            if c_q == 0 {
                run -= 2; // subtract 2, since events number if multiplied by 2
                t0 = if run == -1 { t0 } else { 0 };
                if run < 0 {
                    run = mel.get_run(); // get another run
                }
            }
            scratch[sp] = t0;
            x += 2;

            // prepare context for the next quad; eqn. 1 in ITU T.814
            c_q = ((u32::from(t0) & 0x10) << 3) | ((u32::from(t0) & 0xE0) << 2);

            // remove data from vlc stream (0 bits are removed if vlc is not used)
            vlc_val = vlc.advance(u32::from(t0) & 0x7);

            // second quad
            let mut t1 = t.vlc_tbl0[(c_q + (vlc_val & 0x7F)) as usize];

            // if context is zero, use one MEL event
            if c_q == 0 && x < width {
                run -= 2;
                t1 = if run == -1 { t1 } else { 0 };
                if run < 0 {
                    run = mel.get_run();
                }
            }
            t1 = if x < width { t1 } else { 0 };
            scratch[sp + 2] = t1;
            x += 2;

            // prepare context for the next quad, eqn. 1 in ITU T.814
            c_q = ((u32::from(t1) & 0x10) << 3) | ((u32::from(t1) & 0xE0) << 2);

            // remove data from vlc stream, if qinf is not used, cwdlen is 0
            vlc_val = vlc.advance(u32::from(t1) & 0x7);

            // decode u
            // uvlc_mode is made up of u_offset bits from the quad pair
            let mut uvlc_mode = ((u32::from(t0) & 0x8) << 3) | ((u32::from(t1) & 0x8) << 4);
            if uvlc_mode == 0xc0 {
                // if both u_offset are set, get an event from the MEL run of events
                run -= 2;
                uvlc_mode += if run == -1 { 0x40 } else { 0 };
                if run < 0 {
                    run = mel.get_run();
                }
            }

            // decode uvlc_mode to get u for both quads
            let mut uvlc_entry = u32::from(t.uvlc_tbl0[(uvlc_mode + (vlc_val & 0x3F)) as usize]);
            // remove total prefix length
            vlc_val = vlc.advance(uvlc_entry & 0x7);
            uvlc_entry >>= 3;
            // extract suffixes for quad 0 and 1
            let mut len = uvlc_entry & 0xF; // suffix length for 2 quads
            let tmp = vlc_val & 1u32.wrapping_shl(len).wrapping_sub(1); // suffix value for 2 quads
            vlc_val = vlc.advance(len);
            let _ = vlc_val;
            uvlc_entry >>= 4;
            // quad 0 length
            len = uvlc_entry & 0x7; // quad 0 suffix length
            uvlc_entry >>= 3;
            let mut u_q = (1 + (uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16; // kap. 1
            scratch[sp + 1] = u_q;
            u_q = (1 + (uvlc_entry >> 3) + (tmp >> len)) as u16; // kappa == 1
            scratch[sp + 3] = u_q;
            sp += 4;
        }
        scratch[sp] = 0;
        scratch[sp + 1] = 0;

        // non initial quad rows
        let mut y = 2;
        while y < height {
            c_q = 0; // context
            let mut sp = (y as usize >> 1) * sstr; // this row of quads

            let mut x = 0u32;
            while x < width {
                // sigma_q (n, ne, nf)
                c_q |= (u32::from(scratch[sp - sstr]) & 0xA0) << 2;
                c_q |= (u32::from(scratch[sp + 2 - sstr]) & 0x20) << 4;

                // first quad
                vlc_val = vlc.fetch();

                // decode VLC using the context c_q and the head of VLC bitstream
                let mut t0 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F)) as usize];

                // if context is zero, use one MEL event
                if c_q == 0 {
                    run -= 2;
                    t0 = if run == -1 { t0 } else { 0 };
                    if run < 0 {
                        run = mel.get_run();
                    }
                }
                scratch[sp] = t0;
                x += 2;

                // prepare context for the next quad; eqn. 2 in ITU T.814
                // sigma_q (w, sw)
                c_q = ((u32::from(t0) & 0x40) << 2) | ((u32::from(t0) & 0x80) << 1);
                // sigma_q (nw)
                c_q |= u32::from(scratch[sp - sstr]) & 0x80;
                // sigma_q (n, ne, nf)
                c_q |= (u32::from(scratch[sp + 2 - sstr]) & 0xA0) << 2;
                c_q |= (u32::from(scratch[sp + 4 - sstr]) & 0x20) << 4;

                // remove data from vlc stream (0 bits are removed if vlc is unused)
                vlc_val = vlc.advance(u32::from(t0) & 0x7);

                // second quad
                let mut t1 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F)) as usize];

                // if context is zero, use one MEL event
                if c_q == 0 && x < width {
                    run -= 2;
                    t1 = if run == -1 { t1 } else { 0 };
                    if run < 0 {
                        run = mel.get_run();
                    }
                }
                t1 = if x < width { t1 } else { 0 };
                scratch[sp + 2] = t1;
                x += 2;

                // partial c_q, will be completed when we process the next quad
                // sigma_q (w, sw)
                c_q = ((u32::from(t1) & 0x40) << 2) | ((u32::from(t1) & 0x80) << 1);
                // sigma_q (nw)
                c_q |= u32::from(scratch[sp + 2 - sstr]) & 0x80;

                // remove data from vlc stream, if qinf is not used, cwdlen is 0
                vlc_val = vlc.advance(u32::from(t1) & 0x7);

                // decode u
                // uvlc_mode is made up of u_offset bits from the quad pair
                let uvlc_mode = ((u32::from(t0) & 0x8) << 3) | ((u32::from(t1) & 0x8) << 4);
                let mut uvlc_entry =
                    u32::from(t.uvlc_tbl1[(uvlc_mode + (vlc_val & 0x3F)) as usize]);
                // remove total prefix length
                vlc_val = vlc.advance(uvlc_entry & 0x7);
                uvlc_entry >>= 3;
                // extract suffixes for quad 0 and 1
                let mut len = uvlc_entry & 0xF; // suffix length for 2 quads
                let tmp = vlc_val & 1u32.wrapping_shl(len).wrapping_sub(1); // suffix value
                vlc_val = vlc.advance(len);
                let _ = vlc_val;
                uvlc_entry >>= 4;
                // quad 0 length
                len = uvlc_entry & 0x7; // quad 0 suffix length
                uvlc_entry >>= 3;
                let mut u_q = ((uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16;
                scratch[sp + 1] = u_q;
                u_q = ((uvlc_entry >> 3) + (tmp >> len)) as u16; // u_q
                scratch[sp + 3] = u_q;
                sp += 4;
            }
            scratch[sp] = 0;
            scratch[sp + 1] = 0;
            y += 2;
        }
    }

    // SAFETY (step 2 and the passes): the checks at the top: every load of scratch, the
    // stack arrays and decoded_data stays inside them, as upstream's do
    unsafe {
        let sc = scratch.as_mut_ptr();
        let dd = decoded_data.as_mut_ptr();

        // step2 we decode magsgn
        if mmsbp2 >= 16 {
            let mut v_n_scratch = [0u32; 2 * V_N_SIZE]; // 4+ kB
            let mut magsgn = Frwd::init::<0xFF>(coded_data, 0, lcup - scup);

            {
                let mut sp = sc;
                let mut vp = v_n_scratch.as_mut_ptr();
                let mut dp = dd;
                *vp = 2; // for easy calculation of emax

                let mut x = 0;
                while x < width {
                    // here we process two quads
                    let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                    let u_q = _mm_srli_epi32::<16>(inf_u_q);

                    let w = _mm_cmpgt_epi32(u_q, _mm_set1_epi32(mmsbp2 as i32));
                    let i = _mm_movemask_epi8(w);
                    if (i & 0xFF) != 0 {
                        // only the lower two U_q
                        return Some(false);
                    }

                    let mut vn = _mm_set1_epi32(2);
                    let mut row0 = decode_one_quad32::<0>(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    let mut row1 = decode_one_quad32::<1>(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    let mut w0 = _mm_loadu_si128(vp as *const __m128i);
                    w0 = _mm_and_si128(w0, _mm_set_epi32(0, 0, 0, -1));
                    w0 = _mm_or_si128(w0, vn);
                    _mm_storeu_si128(vp as *mut __m128i, w0);

                    // interleave in ssse3 style
                    w0 = _mm_unpacklo_epi32(row0, row1);
                    let w1 = _mm_unpackhi_epi32(row0, row1);
                    row0 = _mm_unpacklo_epi32(w0, w1);
                    row1 = _mm_unpackhi_epi32(w0, w1);
                    _mm_storeu_si128(dp as *mut __m128i, row0);
                    _mm_storeu_si128(dp.add(stride_u) as *mut __m128i, row1);
                    x += 4;
                    sp = sp.add(4);
                    vp = vp.add(2);
                    dp = dp.add(4);
                }
            }

            let mut y = 2;
            while y < height {
                {
                    // perform 31 - count_leading_zeros(*vp) here
                    let mut vp = v_n_scratch.as_mut_ptr();
                    let lut_lo = _mm_set_epi8(4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 31);
                    let lut_hi = _mm_set_epi8(0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 31);
                    let nibble_mask = _mm_set1_epi8(0x0F);
                    let byte_offset8 = _mm_set1_epi16(8);
                    let byte_offset16 = _mm_set1_epi16(16);
                    let cc = _mm_set1_epi32(31);
                    let mut x = 0;
                    while x <= width {
                        let mut v = _mm_loadu_si128(vp as *const __m128i);

                        let mut t = _mm_and_si128(nibble_mask, v);
                        v = _mm_and_si128(_mm_srli_epi16::<4>(v), nibble_mask);
                        t = _mm_shuffle_epi8(lut_lo, t);
                        v = _mm_shuffle_epi8(lut_hi, v);
                        v = _mm_min_epu8(v, t);

                        t = _mm_srli_epi16::<8>(v);
                        v = _mm_or_si128(v, byte_offset8);
                        v = _mm_min_epu8(v, t);

                        t = _mm_srli_epi32::<16>(v);
                        v = _mm_or_si128(v, byte_offset16);
                        v = _mm_min_epu8(v, t);

                        v = _mm_sub_epi16(cc, v);
                        _mm_storeu_si128(vp.add(V_N_SIZE) as *mut __m128i, v);
                        x += 8;
                        vp = vp.add(4);
                    }
                }

                let mut vp = v_n_scratch.as_mut_ptr();
                let mut sp = sc.add((y as usize >> 1) * sstr);
                let mut dp = dd.add(y as usize * stride_u);
                *vp = 2; // for easy calculation of emax

                let mut x = 0;
                while x < width {
                    // process two quads
                    let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                    let mut gamma = _mm_and_si128(inf_u_q, _mm_set1_epi32(0xF0));
                    let mut w0 = _mm_sub_epi32(gamma, _mm_set1_epi32(1));
                    gamma = _mm_and_si128(gamma, w0);
                    gamma = _mm_cmpeq_epi32(gamma, _mm_setzero_si128());

                    let mut emax = _mm_loadu_si128(vp.add(V_N_SIZE) as *const __m128i);
                    w0 = _mm_bsrli_si128::<4>(emax);
                    emax = _mm_max_epi16(w0, emax); // no max_epi32 in ssse3
                    emax = _mm_andnot_si128(gamma, emax);

                    let mut kappa = _mm_set1_epi32(1);
                    kappa = _mm_max_epi16(emax, kappa); // no max_epi32 in ssse3

                    let u_q0 = _mm_srli_epi32::<16>(inf_u_q);
                    let u_q = _mm_add_epi32(u_q0, kappa);

                    w0 = _mm_cmpgt_epi32(u_q, _mm_set1_epi32(mmsbp2 as i32));
                    let i = _mm_movemask_epi8(w0);
                    if (i & 0xFF) != 0 {
                        // only the lower two U_q
                        return Some(false);
                    }

                    let mut vn = _mm_set1_epi32(2);
                    let mut row0 = decode_one_quad32::<0>(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    let mut row1 = decode_one_quad32::<1>(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    w0 = _mm_loadu_si128(vp as *const __m128i);
                    w0 = _mm_and_si128(w0, _mm_set_epi32(0, 0, 0, -1));
                    w0 = _mm_or_si128(w0, vn);
                    _mm_storeu_si128(vp as *mut __m128i, w0);

                    // interleave in ssse3 style
                    w0 = _mm_unpacklo_epi32(row0, row1);
                    let w1 = _mm_unpackhi_epi32(row0, row1);
                    row0 = _mm_unpacklo_epi32(w0, w1);
                    row1 = _mm_unpackhi_epi32(w0, w1);
                    _mm_storeu_si128(dp as *mut __m128i, row0);
                    _mm_storeu_si128(dp.add(stride_u) as *mut __m128i, row1);
                    x += 4;
                    sp = sp.add(4);
                    vp = vp.add(2);
                    dp = dp.add(4);
                }
                y += 2;
            }
        } else {
            // reduce bitplane by 16 because we now have 16 bits instead of 32
            p -= 16;

            let mut v_n_scratch = [0u16; 2 * V_N_SIZE]; // 2+ kB
            let mut magsgn = Frwd::init::<0xFF>(coded_data, 0, lcup - scup);

            {
                let mut sp = sc;
                let mut vp = v_n_scratch.as_mut_ptr();
                let mut dp = dd;
                *vp = 2; // for easy calculation of emax

                let mut x = 0;
                while x < width {
                    // here we process two quads
                    let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                    let u_q = _mm_srli_epi32::<16>(inf_u_q);

                    let mut w0 = _mm_cmpgt_epi32(u_q, _mm_set1_epi32(mmsbp2 as i32));
                    let i = _mm_movemask_epi8(w0);
                    if (i & 0xFF) != 0 {
                        // only the lower two U_q
                        return Some(false);
                    }

                    let mut vn = _mm_set1_epi16(2);
                    let row = decode_two_quad16(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    w0 = _mm_loadu_si128(vp as *const __m128i);
                    w0 = _mm_and_si128(w0, _mm_set_epi16(0, 0, 0, 0, 0, 0, 0, -1));
                    w0 = _mm_or_si128(w0, vn);
                    _mm_storeu_si128(vp as *mut __m128i, w0);

                    // interleave in ssse3 style
                    w0 = _mm_shuffle_epi8(
                        row,
                        _mm_set_epi16(0x0D0C, -1, 0x0908, -1, 0x0504, -1, 0x0100, -1),
                    );
                    _mm_storeu_si128(dp as *mut __m128i, w0);
                    let w1 = _mm_shuffle_epi8(
                        row,
                        _mm_set_epi16(0x0F0E, -1, 0x0B0A, -1, 0x0706, -1, 0x0302, -1),
                    );
                    _mm_storeu_si128(dp.add(stride_u) as *mut __m128i, w1);
                    x += 4;
                    sp = sp.add(4);
                    vp = vp.add(2);
                    dp = dp.add(4);
                }
            }

            let mut y = 2;
            while y < height {
                {
                    // perform 15 - count_leading_zeros(*vp) here
                    let mut vp = v_n_scratch.as_mut_ptr();
                    let lut_lo = _mm_set_epi8(4, 4, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 15);
                    let lut_hi = _mm_set_epi8(0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 15);
                    let nibble_mask = _mm_set1_epi8(0x0F);
                    let byte_offset8 = _mm_set1_epi16(8);
                    let cc = _mm_set1_epi16(15);
                    let mut x = 0;
                    while x <= width {
                        let mut v = _mm_loadu_si128(vp as *const __m128i);

                        let mut t = _mm_and_si128(nibble_mask, v);
                        v = _mm_and_si128(_mm_srli_epi16::<4>(v), nibble_mask);
                        t = _mm_shuffle_epi8(lut_lo, t);
                        v = _mm_shuffle_epi8(lut_hi, v);
                        v = _mm_min_epu8(v, t);

                        t = _mm_srli_epi16::<8>(v);
                        v = _mm_or_si128(v, byte_offset8);
                        v = _mm_min_epu8(v, t);

                        v = _mm_sub_epi16(cc, v);
                        _mm_storeu_si128(vp.add(V_N_SIZE) as *mut __m128i, v);
                        x += 16;
                        vp = vp.add(8);
                    }
                }

                let mut vp = v_n_scratch.as_mut_ptr();
                let mut sp = sc.add((y as usize >> 1) * sstr);
                let mut dp = dd.add(y as usize * stride_u);
                *vp = 2; // for easy calculation of emax

                let mut x = 0;
                while x < width {
                    // process two quads
                    let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                    let mut gamma = _mm_and_si128(inf_u_q, _mm_set1_epi32(0xF0));
                    let mut w0 = _mm_sub_epi32(gamma, _mm_set1_epi32(1));
                    gamma = _mm_and_si128(gamma, w0);
                    gamma = _mm_cmpeq_epi32(gamma, _mm_setzero_si128());

                    let mut emax = _mm_loadu_si128(vp.add(V_N_SIZE) as *const __m128i);
                    w0 = _mm_bsrli_si128::<2>(emax);
                    emax = _mm_max_epi16(w0, emax); // no max_epi32 in ssse3
                    emax = _mm_shuffle_epi8(
                        emax,
                        _mm_set_epi16(-1, 0x0706, -1, 0x0504, -1, 0x0302, -1, 0x0100),
                    );
                    emax = _mm_andnot_si128(gamma, emax);

                    let mut kappa = _mm_set1_epi32(1);
                    kappa = _mm_max_epi16(emax, kappa); // no max_epi32 in ssse3

                    let u_q0 = _mm_srli_epi32::<16>(inf_u_q);
                    let u_q = _mm_add_epi32(u_q0, kappa);

                    w0 = _mm_cmpgt_epi32(u_q, _mm_set1_epi32(mmsbp2 as i32));
                    let i = _mm_movemask_epi8(w0);
                    if (i & 0xFF) != 0 {
                        // only the lower two U_q
                        return Some(false);
                    }

                    let mut vn = _mm_set1_epi16(2);
                    let row = decode_two_quad16(inf_u_q, u_q, &mut magsgn, p, &mut vn);
                    w0 = _mm_loadu_si128(vp as *const __m128i);
                    w0 = _mm_and_si128(w0, _mm_set_epi16(0, 0, 0, 0, 0, 0, 0, -1));
                    w0 = _mm_or_si128(w0, vn);
                    _mm_storeu_si128(vp as *mut __m128i, w0);

                    w0 = _mm_shuffle_epi8(
                        row,
                        _mm_set_epi16(0x0D0C, -1, 0x0908, -1, 0x0504, -1, 0x0100, -1),
                    );
                    _mm_storeu_si128(dp as *mut __m128i, w0);
                    let w1 = _mm_shuffle_epi8(
                        row,
                        _mm_set_epi16(0x0F0E, -1, 0x0B0A, -1, 0x0706, -1, 0x0302, -1),
                    );
                    _mm_storeu_si128(dp.add(stride_u) as *mut __m128i, w1);
                    x += 4;
                    sp = sp.add(4);
                    vp = vp.add(2);
                    dp = dp.add(4);
                }
                y += 2;
            }

            // increase bitplane back by 16 because we need to process 32 bits
            p += 16;
        }

        if num_passes > 1 {
            // sigma holds all the significant samples, and it cannot be modified after it is
            // set. it will be used during the Magnitude Refinement Pass
            let sigma = sc;

            let mut mstr = ((width + 3) >> 2) as usize; // divide by 4
            mstr = ((mstr + 2) + 7) & !7; // multiples of 8

            // We re-arrange quad significance into column significance
            {
                let mask_3 = _mm_set1_epi32(0x30);
                let mask_c = _mm_set1_epi32(0xC0);
                let shuffle_mask = _mm_set_epi32(-1, -1, -1, 0x0C080400);
                let mut y = 0;
                while y < height {
                    let mut sp = sc.add((y as usize >> 1) * sstr);
                    let mut dp = sigma.add((y as usize >> 2) * mstr);
                    let mut x = 0;
                    while x < width {
                        let s0 = _mm_loadu_si128(sp as *const __m128i);
                        let mut u3 = _mm_and_si128(s0, mask_3);
                        u3 = _mm_srli_epi32::<4>(u3);
                        let mut uc = _mm_and_si128(s0, mask_c);
                        uc = _mm_srli_epi32::<2>(uc);
                        let t0 = _mm_or_si128(u3, uc);

                        let s1 = _mm_loadu_si128(sp.add(sstr) as *const __m128i);
                        u3 = _mm_and_si128(s1, mask_3);
                        u3 = _mm_srli_epi32::<2>(u3);
                        uc = _mm_and_si128(s1, mask_c);
                        let t1 = _mm_or_si128(u3, uc);

                        let mut r = _mm_or_si128(t0, t1);
                        r = _mm_shuffle_epi8(r, shuffle_mask);

                        *dp = _mm_extract_epi16::<0>(r) as u16;
                        *dp.add(1) = _mm_extract_epi16::<1>(r) as u16;
                        x += 8;
                        sp = sp.add(8);
                        dp = dp.add(2);
                    }
                    *dp = 0; // set an extra entry on the right with 0
                    y += 4;
                }
                {
                    // reset one row after the codeblock
                    let mut dp = sigma.add((y as usize >> 2) * mstr);
                    let zero = _mm_setzero_si128();
                    let mut x = 0;
                    while x < width {
                        _mm_storeu_si128(dp as *mut __m128i, zero);
                        x += 32;
                        dp = dp.add(8);
                    }
                    *dp = 0; // set an extra entry on the right with 0
                }
            }

            // We perform Significance Propagation Pass here
            {
                let mut prev_row_sig = [0u16; 256 + 8]; // 528 Bytes

                let mut sigprop = Frwd::init::<0>(coded_data, lengths1 as usize, lengths2 as i32);

                let mut y = 0;
                while y < height {
                    let mut pattern: u32 = 0xFFFF; // a pattern needed samples
                    if height - y < 4 {
                        pattern = 0x7777;
                        if height - y < 3 {
                            pattern = 0x3333;
                            if height - y < 2 {
                                pattern = 0x1111;
                            }
                        }
                    }

                    // prev holds sign. info. for the previous quad, together with the rows on
                    // top of it and below it.
                    let mut prev: u32 = 0;
                    let mut prev_sig = prev_row_sig.as_mut_ptr();
                    let mut cur_sig = sigma.add((y as usize >> 2) * mstr);
                    let mut dpp = dd.add(y as usize * stride_u);
                    let mut x = 0;
                    while x < width {
                        // only rows and columns inside the stripe are included
                        let mut s = x as i32 + 4 - width as i32;
                        s = if s > 0 { s } else { 0 };
                        pattern >>= s * 4;

                        let ps = (prev_sig as *const u32).read_unaligned();
                        let ns = (cur_sig.add(mstr) as *const u32).read_unaligned();
                        let mut u = (ps & 0x88888888) >> 3; // the row on top
                        if !stripe_causal {
                            u |= (ns & 0x11111111) << 3; // the row below
                        }

                        let cs = (cur_sig as *const u32).read_unaligned();
                        // vertical integration
                        let mut mbr = cs; // this sig. info.
                        mbr |= (cs & 0x77777777) << 1; // above neighbors
                        mbr |= (cs & 0xEEEEEEEE) >> 1; // below neighbors
                        mbr |= u;
                        // horizontal integration
                        let t = mbr;
                        mbr |= t << 4; // neighbors on the left
                        mbr |= t >> 4; // neighbors on the right
                        mbr |= prev >> 12; // significance of previous group

                        // remove outside samples, and already significant samples
                        mbr &= pattern;
                        mbr &= !cs;

                        // find samples that become significant during the SPP
                        let mut new_sig = mbr;
                        if new_sig != 0 {
                            let cwd_vec = sigprop.fetch::<0>();
                            let mut cwd = _mm_extract_epi16::<0>(cwd_vec) as u32;

                            let mut cnt: u32 = 0;
                            let mut col_mask: u32 = 0xF;
                            let inv_sig = !cs & pattern;
                            let mut i = 0;
                            while i < 16 {
                                if (col_mask & new_sig) != 0 {
                                    // scan one column
                                    let mut sample_mask = 0x1111 & col_mask;
                                    for t in [0x33u32, 0x76, 0xEC, 0xC8] {
                                        if new_sig & sample_mask != 0 {
                                            new_sig &= !sample_mask;
                                            if cwd & 1 != 0 {
                                                new_sig |= (t << i) & inv_sig;
                                            }
                                            cwd >>= 1;
                                            cnt += 1;
                                        }
                                        sample_mask <<= 1;
                                    }
                                }
                                i += 4;
                                col_mask <<= 4;
                            }

                            if new_sig != 0 {
                                cwd |= (_mm_extract_epi16::<1>(cwd_vec) as u32) << (16 - cnt);

                                let new_sig_vec = spread16(new_sig);

                                // find cumulative sums to find which bit in cwd we should
                                // extract
                                let mut inc_sum = new_sig_vec; // inclusive scan
                                inc_sum = _mm_abs_epi8(inc_sum); // cvrt to 0 or 1
                                inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<1>(inc_sum));
                                inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<2>(inc_sum));
                                inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<4>(inc_sum));
                                inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<8>(inc_sum));
                                cnt += (_mm_extract_epi16::<7>(inc_sum) as u32) >> 8;
                                // exclusive scan
                                let ex_sum = _mm_bslli_si128::<1>(inc_sum);

                                // Spread cwd, such that each bit is in one byte with a value
                                // of 0 or 1.
                                let cwd_vec = _mm_abs_epi8(spread16(cwd));

                                // Obtain bit from cwd_vec correspondig to ex_sum
                                let v = _mm_shuffle_epi8(cwd_vec, ex_sum);

                                // load data and set spp coefficients
                                let mut m = _mm_set_epi8(
                                    -1, -1, -1, 12, -1, -1, -1, 8, -1, -1, -1, 4, -1, -1, -1, 0,
                                );
                                let val = _mm_set1_epi32(3 << (p - 2));
                                let mut dp = dpp;
                                for _ in 0..4 {
                                    // load coefficients
                                    let mut s0 = _mm_loadu_si128(dp as *const __m128i);

                                    // epi32 is -1 only for coefficient that are changed during
                                    // the SPP
                                    let mut s0_ns = _mm_shuffle_epi8(new_sig_vec, m);
                                    s0_ns = _mm_cmpeq_epi32(s0_ns, _mm_set1_epi32(0xFF));

                                    // obtain sign for coefficients in SPP
                                    let mut s0_val = _mm_shuffle_epi8(v, m);
                                    s0_val = _mm_slli_epi32::<31>(s0_val);
                                    s0_val = _mm_or_si128(s0_val, val);
                                    s0_val = _mm_and_si128(s0_val, s0_ns);

                                    // update vector
                                    s0 = _mm_or_si128(s0, s0_val);
                                    // store coefficients
                                    _mm_storeu_si128(dp as *mut __m128i, s0);
                                    // prepare for next row
                                    dp = dp.add(stride_u);
                                    m = _mm_add_epi32(m, _mm_set1_epi32(1));
                                }
                            }
                            sigprop.advance(cnt);
                        }

                        new_sig |= cs;
                        *prev_sig = new_sig as u16;

                        // vertical integration for the new sig. info.
                        let t = new_sig;
                        new_sig |= (t & 0x7777) << 1; // above neighbors
                        new_sig |= (t & 0xEEEE) >> 1; // below neighbors
                        // add sig. info. from the row on top and below
                        prev = new_sig | u;
                        // we need only the bits in 0xF000
                        prev &= 0xF000;
                        x += 4;
                        dpp = dpp.add(4);
                        cur_sig = cur_sig.add(1);
                        prev_sig = prev_sig.add(1);
                    }
                    y += 4;
                }
            }

            // We perform Magnitude Refinement Pass here
            if num_passes > 2 {
                let mut magref = Rev::init_mrp(coded_data, lengths1 as i32, lengths2 as i32);

                let mut y = 0;
                while y < height {
                    let mut cur_sig = sigma.add((y as usize >> 2) * mstr);
                    let mut dpp = dd.add(y as usize * stride_u);
                    let mut i = 0;
                    while i < width {
                        // Process one entry from sigma array at a time
                        let cwd = magref.fetch(); // get 32 bit data
                        let sig = *cur_sig; // 16 bit that will be processed now
                        cur_sig = cur_sig.add(1);
                        let mut total_bits: i32 = 0;
                        if sig != 0 {
                            // spread the 16 bits in sig to 0 or 1 bytes in sig_vec
                            let sig_vec = _mm_abs_epi8(spread16(u32::from(sig)));

                            // find cumulative sums to find which bit in cwd we should extract
                            let mut inc_sum = sig_vec; // inclusive scan
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<1>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<2>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<4>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<8>(inc_sum));
                            total_bits = _mm_extract_epi16::<7>(inc_sum) >> 8;
                            let ex_sum = _mm_bslli_si128::<1>(inc_sum); // exclusive scan

                            // a value of 0 as binary 11, and a value of 1 as binary 01
                            let mut cwd_vec = spread16(cwd);
                            cwd_vec = _mm_add_epi8(cwd_vec, _mm_set1_epi8(1));
                            cwd_vec = _mm_add_epi8(cwd_vec, cwd_vec);
                            cwd_vec = _mm_or_si128(cwd_vec, _mm_set1_epi8(1));

                            // load data and insert the mrp bit
                            let mut m = _mm_set_epi8(
                                -1, -1, -1, 12, -1, -1, -1, 8, -1, -1, -1, 4, -1, -1, -1, 0,
                            );
                            let mut dp = dpp;
                            for _ in 0..4 {
                                // load coefficients
                                let mut s0 = _mm_loadu_si128(dp as *const __m128i);
                                // find significant samples in this row
                                let mut s0_sig = _mm_shuffle_epi8(sig_vec, m);
                                s0_sig = _mm_cmpeq_epi8(s0_sig, _mm_setzero_si128());
                                // get MRP bit index, and MRP pattern
                                let s0_idx = _mm_shuffle_epi8(ex_sum, m);
                                let mut s0_val = _mm_shuffle_epi8(cwd_vec, s0_idx);
                                // keep data from significant samples only
                                s0_val = _mm_andnot_si128(s0_sig, s0_val);
                                // move mrp bits to correct position, and employ
                                s0_val = _mm_sll_epi32(s0_val, _mm_cvtsi32_si128(p as i32 - 2));
                                s0 = _mm_xor_si128(s0, s0_val);
                                // store coefficients
                                _mm_storeu_si128(dp as *mut __m128i, s0);
                                // prepare for next row
                                dp = dp.add(stride_u);
                                m = _mm_add_epi32(m, _mm_set1_epi32(1));
                            }
                        }
                        // consume data according to the number of bits set
                        magref.advance(total_bits as u32);
                        i += 4;
                        dpp = dpp.add(4);
                    }
                    y += 4;
                }
            }
        }
    }

    Some(true)
}

/// [`ojph_decode_codeblock_ssse3`] where the CPU has SSSE3 (upstream's level check,
/// `get_cpu_ext_level() >= X86_CPU_EXT_LEVEL_SSSE3`, implies it); `None` otherwise, or when
/// the buffers cannot hold its accesses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_codeblock_ssse3(
    coded_data: &[u8],
    decoded_data: &mut [u32],
    missing_msbs: u32,
    num_passes: u32,
    lengths1: u32,
    lengths2: u32,
    width: u32,
    height: u32,
    stride: u32,
    stripe_causal: bool,
) -> Option<bool> {
    if !std::arch::is_x86_feature_detected!("ssse3") {
        return None;
    }
    // SAFETY: the CPU has SSSE3
    unsafe {
        ojph_decode_codeblock_ssse3(
            coded_data,
            decoded_data,
            missing_msbs,
            num_passes,
            lengths1,
            lengths2,
            width,
            height,
            stride,
            stripe_causal,
        )
    }
}
