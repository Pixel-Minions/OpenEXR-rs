// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_decoder_avx2.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:1-1976 @ OpenJPH 0.32.0): the
//! HT block decoder upstream chooses at `X86_CPU_EXT_LEVEL_AVX2` and above
//! (`codeblock_fun::init`), in AVX2. Its streams are first de-stuffed into flat buffers and
//! read at absolute bit positions; the MEL decoder is the generic one's
//! ([`crate::block_decoder32::DecMel`], the same code upstream).
//!
//! Memory: upstream reads and writes past the codeblock's width and height, into the
//! codeblock buffer's padding (stores of 8 samples, rows of 4) and into its stack scratch
//! arrays; [`ojph_decode_codeblock_avx2`] checks once that the caller's buffers hold every
//! such access before any pointer is used, and its scratch arrays are sized as upstream's
//! (with the same regions zeroed; nothing outside what upstream writes or zeroes is read).

#![allow(unsafe_code)]

use std::arch::x86_64::*;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::block_common::tables;
use crate::block_decoder32::DecMel;
use crate::message::{Arg, warn};

const FILE: &str = "ojph_block_decoder_avx2.cpp";

/// Port of `destuff_frwd` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:290-359
/// @ OpenJPH 0.32.0): the forward stream at `src` (`size` bytes) de-stuffed into `dst`, at
/// most `cap + 1` bytes and then 64 bytes of `X`; returns the clamp offset.
///
/// # Safety
/// `src` holds `size` bytes (when positive) and `dst` holds `cap + 65` bytes.
#[target_feature(enable = "avx2")]
unsafe fn destuff_frwd<const X: u32>(src: *const u8, size: i32, dst: *mut u8, cap: u32) -> u32 {
    // SAFETY (whole function): every pointer stays in the ranges the caller promises: reads
    // below `s_end`, writes below `o_end + 1 + 64`.
    unsafe {
        let size = if size < 0 { 0 } else { size };
        let mut o = dst;
        let o_end = dst.add(cap as usize);
        let mut s = src;
        let s_end = src.add(size as usize);
        let mut acc: u64 = 0; // partial output byte, low nb bits are valid
        let mut nb: u32 = 0; // number of valid bits in acc; always < 8
        let mut prev_ff = false;

        // fast path; 16 source bytes at a time when they contain no 0xFF
        while s.add(16) <= s_end && o.add(24) <= o_end {
            let v = _mm_loadu_si128(s as *const __m128i);
            let ff = _mm_movemask_epi8(_mm_cmpeq_epi8(v, _mm_set1_epi8(-1)));
            if ff != 0 || prev_ff {
                // process these 16 bytes one at a time
                for _ in 0..16 {
                    let b = *s;
                    s = s.add(1);
                    acc |= u64::from(b) << nb;
                    nb += if prev_ff { 7 } else { 8 };
                    prev_ff = b == 0xFF;
                    if nb >= 8 {
                        *o = acc as u8;
                        o = o.add(1);
                        acc >>= 8;
                        nb -= 8;
                    }
                }
                continue;
            }
            let v0 = (s as *const u64).read_unaligned();
            let v1 = (s.add(8) as *const u64).read_unaligned();
            let w0 = acc | (v0 << nb);
            let w1 = (v1 << nb) | if nb != 0 { v0 >> (64 - nb) } else { 0 };
            (o as *mut u64).write_unaligned(w0);
            (o.add(8) as *mut u64).write_unaligned(w1);
            acc = if nb != 0 { v1 >> (64 - nb) } else { 0 };
            o = o.add(16);
            s = s.add(16);
        }
        // tail; one byte at a time
        while s < s_end && o < o_end {
            let b = *s;
            s = s.add(1);
            acc |= u64::from(b) << nb;
            nb += if prev_ff { 7 } else { 8 };
            prev_ff = b == 0xFF;
            if nb >= 8 {
                *o = acc as u8;
                o = o.add(1);
                acc >>= 8;
                nb -= 8;
            }
        }
        // fill the bits above nb with X, and pad with X bytes
        let fill: u32 = if X == 0xFF { 0xFFu32 << nb } else { 0 };
        *o = ((acc as u32) | fill) as u8;
        let pad = _mm256_set1_epi8(X as u8 as i8);
        _mm256_storeu_si256(o.add(1) as *mut __m256i, pad);
        _mm256_storeu_si256(o.add(33) as *mut __m256i, pad);
        (o.offset_from(dst) as u32) + 1
    }
}

/// Port of `dfetch` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:375-387
/// @ OpenJPH 0.32.0): the 128 bits at bit `pos` of a de-stuffed buffer, its offset clamped.
///
/// # Safety
/// `dbuf` holds `limit + 24` bytes.
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn dfetch(dbuf: *const u8, limit: u32, pos: u32) -> __m128i {
    let mut off = pos >> 3;
    off = if off < limit { off } else { limit };
    // SAFETY: off <= limit, and the caller's buffer holds limit + 24 bytes
    let (v, w) = unsafe {
        let p = dbuf.add(off as usize);
        (
            _mm_loadu_si128(p as *const __m128i),
            _mm_loadu_si128(p.add(8) as *const __m128i),
        )
    };
    let k = (pos & 7) as i32;
    let r = _mm_srl_epi64(v, _mm_cvtsi32_si128(k));
    let c = _mm_sll_epi64(w, _mm_cvtsi32_si128(64 - k));
    _mm_or_si128(r, c)
}

/// Port of `dfetch64` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:400-408
/// @ OpenJPH 0.32.0).
///
/// # Safety
/// `dbuf` holds `limit + 8` bytes.
#[inline]
unsafe fn dfetch64(dbuf: *const u8, limit: u32, pos: u32) -> u64 {
    let mut off = pos >> 3;
    off = if off < limit { off } else { limit };
    // SAFETY: off <= limit, and the caller's buffer holds limit + 8 bytes
    let v = unsafe { (dbuf.add(off as usize) as *const u64).read_unaligned() };
    v >> (pos & 7)
}

/// Port of `drefill` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:429-439
/// @ OpenJPH 0.32.0).
///
/// # Safety
/// `dbuf` holds `limit + 8` bytes.
#[inline]
unsafe fn drefill(val: &mut u64, bits: &mut u32, off: &mut u32, dbuf: *const u8, limit: u32) {
    let o = if *off < limit { *off } else { limit };
    // SAFETY: o <= limit, and the caller's buffer holds limit + 8 bytes
    let v = unsafe { (dbuf.add(o as usize) as *const u64).read_unaligned() };
    *val |= v.wrapping_shl(*bits);
    *off += (63 - *bits) >> 3;
    *bits |= 56;
}

/// Port of `dconsume` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:448-453
/// @ OpenJPH 0.32.0).
#[inline]
fn dconsume(val: &mut u64, bits: &mut u32, num_bits: u32) {
    *val >>= num_bits;
    *bits = bits.wrapping_sub(num_bits);
}

/// Port of `destuff_rev` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:485-567
/// @ OpenJPH 0.32.0): a backward stream, its last byte at `p`, de-stuffed into `dst`.
///
/// # Safety
/// `p` and the `size - 1` bytes before it are readable (with `p[1]` when `size >= 16`), and
/// `dst` holds `cap + 65` bytes.
#[target_feature(enable = "avx2")]
unsafe fn destuff_rev(
    p: *const u8,
    size: i32,
    unstuff: bool,
    acc: u64,
    nb: u32,
    dst: *mut u8,
    cap: u32,
) -> u32 {
    // SAFETY (whole function): reads stay in the segment ending at `p` (and its byte after,
    // which the caller's segment holds), writes below `dst + cap + 65`.
    unsafe {
        let mut p = p;
        let mut size = size;
        let mut unstuff = unstuff;
        let mut acc = acc;
        let mut nb = nb;
        let mut o = dst;
        let o_end = dst.add(cap as usize);

        // process the first byte with the caller-provided unstuff state
        if size > 0 && o < o_end {
            let d = u32::from(*p);
            p = p.wrapping_sub(1);
            size -= 1;
            acc |= u64::from(d) << nb;
            nb += 8 - if unstuff && (d & 0x7F) == 0x7F { 1 } else { 0 };
            unstuff = d > 0x8F;
            if nb >= 8 {
                *o = acc as u8;
                o = o.add(1);
                acc >>= 8;
                nb -= 8;
            }
        }

        // fast path; 16 source bytes at a time when none needs unstuffing
        while size >= 16 && o.add(24) <= o_end {
            let v = _mm_loadu_si128(p.sub(15) as *const __m128i);
            let nx = _mm_loadu_si128(p.sub(14) as *const __m128i);
            let is7f = _mm_cmpeq_epi8(_mm_and_si128(v, _mm_set1_epi8(0x7F)), _mm_set1_epi8(0x7F));
            // le8f is 0xFF where the byte after (in memory) is <= 0x8F
            let le8f = _mm_cmpeq_epi8(
                _mm_subs_epu8(nx, _mm_set1_epi8(0x8F_u8 as i8)),
                _mm_setzero_si128(),
            );
            let stuff = _mm_andnot_si128(le8f, is7f);
            if _mm_testz_si128(stuff, stuff) == 0 {
                // process these 16 bytes one at a time
                for _ in 0..16 {
                    let d = u32::from(*p);
                    p = p.wrapping_sub(1);
                    acc |= u64::from(d) << nb;
                    nb += 8 - if unstuff && (d & 0x7F) == 0x7F { 1 } else { 0 };
                    unstuff = d > 0x8F;
                    if nb >= 8 {
                        *o = acc as u8;
                        o = o.add(1);
                        acc >>= 8;
                        nb -= 8;
                    }
                }
                size -= 16;
                continue;
            }
            let r = _mm_shuffle_epi8(
                v,
                _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15),
            ); // reverse bytes
            let v0 = _mm_cvtsi128_si64(r) as u64;
            let v1 = _mm_extract_epi64::<1>(r) as u64;
            let w0 = acc | (v0 << nb);
            let w1 = (v1 << nb) | if nb != 0 { v0 >> (64 - nb) } else { 0 };
            (o as *mut u64).write_unaligned(w0);
            (o.add(8) as *mut u64).write_unaligned(w1);
            acc = if nb != 0 { v1 >> (64 - nb) } else { 0 };
            o = o.add(16);
            p = p.wrapping_sub(16);
            size -= 16;
            unstuff = *p.add(1) > 0x8F;
        }
        // tail; one byte at a time
        while size > 0 && o < o_end {
            let d = u32::from(*p);
            p = p.wrapping_sub(1);
            size -= 1;
            acc |= u64::from(d) << nb;
            nb += 8 - if unstuff && (d & 0x7F) == 0x7F { 1 } else { 0 };
            unstuff = d > 0x8F;
            if nb >= 8 {
                *o = acc as u8;
                o = o.add(1);
                acc >>= 8;
                nb -= 8;
            }
        }
        // the bits above nb are already 0 = fill; pad with zero bytes
        *o = acc as u8;
        let z = _mm256_setzero_si256();
        _mm256_storeu_si256(o.add(1) as *mut __m256i, z);
        _mm256_storeu_si256(o.add(33) as *mut __m256i, z);
        (o.offset_from(dst) as u32) + 1
    }
}

/// Port of `destuff_vlc` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:585-595
/// @ OpenJPH 0.32.0).
///
/// # Safety
/// `data` holds `lcup` bytes, `2 <= scup <= lcup`, and `dst` holds `cap + 65` bytes.
#[target_feature(enable = "avx2")]
unsafe fn destuff_vlc(data: *const u8, lcup: i32, scup: i32, dst: *mut u8, cap: u32) -> u32 {
    // SAFETY: the segment is the scup - 1 bytes ending at data + lcup - 2
    unsafe {
        let p = data.add((lcup - 2) as usize);
        let d = u32::from(*p); // first byte, only the upper 4 bits are used
        let acc = u64::from(d >> 4);
        let nb = 4 - u32::from((acc & 7) == 7); // check standard
        #[allow(clippy::bad_bit_mask, clippy::ineffective_bit_mask)] // as upstream writes it
        let unstuff = (d | 0xF) > 0x8F;
        destuff_rev(p.wrapping_sub(1), scup - 2, unstuff, acc, nb, dst, cap)
    }
}

/// Port of `destuff_mrp` (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:612-618
/// @ OpenJPH 0.32.0).
///
/// # Safety
/// `data` holds `lcup + len2` bytes, and `dst` holds `cap + 65` bytes.
#[target_feature(enable = "avx2")]
unsafe fn destuff_mrp(data: *const u8, lcup: i32, len2: i32, dst: *mut u8, cap: u32) -> u32 {
    // SAFETY: the segment is the len2 bytes ending at data + lcup + len2 - 1
    unsafe {
        destuff_rev(
            data.wrapping_add((lcup + len2 - 1) as usize),
            len2,
            true,
            0,
            0,
            dst,
            cap,
        )
    }
}

/// Port of `decode_two_quad32_avx2`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:630-726 @ OpenJPH 0.32.0).
///
/// # Safety
/// `dbuf` holds `limit + 24` bytes.
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn decode_two_quad32_avx2(
    inf_u_q: __m256i,
    u_q: __m256i,
    dbuf: *const u8,
    limit: u32,
    pos: &mut u32,
    p: u32,
    vn: &mut __m128i,
) -> __m256i {
    let mut row = _mm256_setzero_si256();

    // we keeps e_k, e_1, and rho in w2
    let mut flags = _mm256_and_si256(
        inf_u_q,
        _mm256_set_epi32(
            0x8880, 0x4440, 0x2220, 0x1110, 0x8880, 0x4440, 0x2220, 0x1110,
        ),
    );
    let insig = _mm256_cmpeq_epi32(flags, _mm256_setzero_si256());

    if _mm256_movemask_epi8(insig) as u32 != 0xFFFFFFFF {
        // are all insignificant?
        flags = _mm256_mullo_epi16(
            flags,
            _mm256_set_epi16(1, 1, 2, 2, 4, 4, 8, 8, 1, 1, 2, 2, 4, 4, 8, 8),
        );

        // next e_k and m_n
        let mut w0 = _mm256_srli_epi32::<15>(flags); // e_k
        let mut m_n = _mm256_sub_epi32(u_q, w0);
        m_n = _mm256_andnot_si256(insig, m_n);

        // find cumulative sums to find at which bit in ms_vec the sample starts
        let mut inc_sum = m_n; // inclusive scan
        inc_sum = _mm256_add_epi32(inc_sum, _mm256_bslli_epi128::<4>(inc_sum));
        inc_sum = _mm256_add_epi32(inc_sum, _mm256_bslli_epi128::<8>(inc_sum));
        let total_mn1 = _mm256_extract_epi16::<6>(inc_sum) as u32;
        let total_mn2 = _mm256_extract_epi16::<14>(inc_sum) as u32;

        // SAFETY: the caller's buffer holds limit + 24 bytes
        let (ms_vec0, ms_vec1) = unsafe {
            (
                dfetch(dbuf, limit, *pos),
                dfetch(dbuf, limit, pos.wrapping_add(total_mn1)),
            )
        };
        *pos = pos.wrapping_add(total_mn1).wrapping_add(total_mn2);

        let mut ms_vec = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(ms_vec0), ms_vec1);

        let ex_sum = _mm256_bslli_epi128::<4>(inc_sum); // exclusive scan

        // find the starting byte and starting bit
        let mut byte_idx = _mm256_srli_epi32::<3>(ex_sum);
        let mut bit_idx = _mm256_and_si256(ex_sum, _mm256_set1_epi32(7));
        byte_idx = _mm256_shuffle_epi8(
            byte_idx,
            _mm256_set_epi32(
                0x0C0C0C0C, 0x08080808, 0x04040404, 0x00000000, 0x0C0C0C0C, 0x08080808, 0x04040404,
                0x00000000,
            ),
        );
        byte_idx = _mm256_add_epi32(byte_idx, _mm256_set1_epi32(0x03020100));
        let mut d0 = _mm256_shuffle_epi8(ms_vec, byte_idx);
        byte_idx = _mm256_add_epi32(byte_idx, _mm256_set1_epi32(0x01010101));
        let mut d1 = _mm256_shuffle_epi8(ms_vec, byte_idx);

        // shift samples values to correct location
        bit_idx = _mm256_or_si256(bit_idx, _mm256_slli_epi32::<16>(bit_idx));

        let a = _mm_set_epi8(1, 3, 7, 15, 31, 63, 127, -1, 1, 3, 7, 15, 31, 63, 127, -1);
        let aa = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(a), a);

        let mut bit_shift = _mm256_shuffle_epi8(aa, bit_idx);
        bit_shift = _mm256_add_epi16(bit_shift, _mm256_set1_epi16(0x0101));
        d0 = _mm256_mullo_epi16(d0, bit_shift);
        d0 = _mm256_srli_epi16::<8>(d0); // we should have 8 bits in the LSB
        d1 = _mm256_mullo_epi16(d1, bit_shift);
        d1 = _mm256_and_si256(d1, _mm256_set1_epi32(0xFF00FF00_u32 as i32)); // 8 in MSB
        d0 = _mm256_or_si256(d0, d1);

        // find location of e_k and mask
        let ones = _mm256_set1_epi32(1);
        let twos = _mm256_set1_epi32(2);
        let mut u_q_m1 = _mm256_sub_epi32(u_q, ones);
        u_q_m1 = _mm256_and_si256(u_q_m1, _mm256_set_epi32(0, 0, 0, 0x1F, 0, 0, 0, 0x1F));
        u_q_m1 = _mm256_shuffle_epi32::<0>(u_q_m1);
        w0 = _mm256_sub_epi32(twos, w0);
        let shift = _mm256_sllv_epi32(w0, u_q_m1); // U_q_m1 must be no more than 31
        ms_vec = _mm256_and_si256(d0, _mm256_sub_epi32(shift, ones));

        // next e_1
        w0 = _mm256_and_si256(flags, _mm256_set1_epi32(0x800));
        w0 = _mm256_cmpeq_epi32(w0, _mm256_setzero_si256());
        w0 = _mm256_andnot_si256(w0, shift); // e_1 in correct position
        ms_vec = _mm256_or_si256(ms_vec, w0); // e_1
        w0 = _mm256_slli_epi32::<31>(ms_vec); // sign
        ms_vec = _mm256_or_si256(ms_vec, ones); // bin center
        let mut tvn = ms_vec;
        ms_vec = _mm256_add_epi32(ms_vec, twos); // + 2
        ms_vec = _mm256_sll_epi32(ms_vec, _mm_cvtsi32_si128(p as i32 - 1));
        ms_vec = _mm256_or_si256(ms_vec, w0); // sign
        row = _mm256_andnot_si256(insig, ms_vec); // significant only

        ms_vec = _mm256_andnot_si256(insig, tvn); // significant only

        tvn = _mm256_shuffle_epi8(
            ms_vec,
            _mm256_set_epi32(
                -1, 0x0F0E0D0C, 0x07060504, -1, -1, -1, 0x0F0E0D0C, 0x07060504,
            ),
        );

        *vn = _mm_or_si128(*vn, _mm256_castsi256_si128(tvn));
        *vn = _mm_or_si128(*vn, _mm256_extracti128_si256::<1>(tvn));
    }
    row
}

/// Port of `decode_four_quad16`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:740-867 @ OpenJPH 0.32.0).
///
/// # Safety
/// `dbuf` holds `limit + 24` bytes.
#[target_feature(enable = "avx2")]
#[inline]
unsafe fn decode_four_quad16(
    inf_u_q: __m128i,
    u_q: __m128i,
    dbuf: *const u8,
    limit: u32,
    pos: &mut u32,
    p: u32,
    vn: &mut __m128i,
) -> __m256i {
    let mut row = _mm256_setzero_si256();

    let mut ddd = _mm_shuffle_epi8(
        inf_u_q,
        _mm_set_epi16(
            0x0d0c, 0x0d0c, 0x0908, 0x908, 0x0504, 0x0504, 0x0100, 0x0100,
        ),
    );
    let mut w0 = _mm256_permutevar8x32_epi32(
        _mm256_castsi128_si256(ddd),
        _mm256_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3),
    );
    // we keeps e_k, e_1, and rho in w2
    let mut flags = _mm256_and_si256(
        w0,
        _mm256_set_epi16(
            0x8880_u16 as i16,
            0x4440,
            0x2220,
            0x1110,
            0x8880_u16 as i16,
            0x4440,
            0x2220,
            0x1110,
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
    let insig = _mm256_cmpeq_epi16(flags, _mm256_setzero_si256());
    if _mm256_movemask_epi8(insig) as u32 != 0xFFFFFFFF {
        // are all insignificant?
        ddd = _mm_or_si128(_mm_bslli_si128::<2>(u_q), u_q);
        let u_q_avx = _mm256_permutevar8x32_epi32(
            _mm256_castsi128_si256(ddd),
            _mm256_setr_epi32(0, 0, 1, 1, 2, 2, 3, 3),
        );
        flags = _mm256_mullo_epi16(
            flags,
            _mm256_set_epi16(1, 2, 4, 8, 1, 2, 4, 8, 1, 2, 4, 8, 1, 2, 4, 8),
        );

        // next e_k and m_n
        w0 = _mm256_srli_epi16::<15>(flags); // e_k
        let mut m_n = _mm256_sub_epi16(u_q_avx, w0);
        m_n = _mm256_andnot_si256(insig, m_n);

        // find cumulative sums to find at which bit in ms_vec the sample starts
        let mut inc_sum = m_n; // inclusive scan
        inc_sum = _mm256_add_epi16(inc_sum, _mm256_bslli_epi128::<2>(inc_sum));
        inc_sum = _mm256_add_epi16(inc_sum, _mm256_bslli_epi128::<4>(inc_sum));
        inc_sum = _mm256_add_epi16(inc_sum, _mm256_bslli_epi128::<8>(inc_sum));
        let total_mn1 = _mm256_extract_epi16::<7>(inc_sum) as u32;
        let total_mn2 = _mm256_extract_epi16::<15>(inc_sum) as u32;
        let ex_sum = _mm256_bslli_epi128::<2>(inc_sum); // exclusive scan

        // SAFETY: the caller's buffer holds limit + 24 bytes
        let (ms_vec0, ms_vec1) = unsafe {
            (
                dfetch(dbuf, limit, *pos),
                dfetch(dbuf, limit, pos.wrapping_add(total_mn1)),
            )
        };
        *pos = pos.wrapping_add(total_mn1).wrapping_add(total_mn2);

        let mut ms_vec = _mm256_inserti128_si256::<1>(_mm256_castsi128_si256(ms_vec0), ms_vec1);

        // find the starting byte and starting bit
        let mut byte_idx = _mm256_srli_epi16::<3>(ex_sum);
        let bit_idx = _mm256_and_si256(ex_sum, _mm256_set1_epi16(7));
        byte_idx = _mm256_shuffle_epi8(
            byte_idx,
            _mm256_set_epi16(
                0x0E0E, 0x0C0C, 0x0A0A, 0x0808, 0x0606, 0x0404, 0x0202, 0x0000, 0x0E0E, 0x0C0C,
                0x0A0A, 0x0808, 0x0606, 0x0404, 0x0202, 0x0000,
            ),
        );
        byte_idx = _mm256_add_epi16(byte_idx, _mm256_set1_epi16(0x0100));
        let mut d0 = _mm256_shuffle_epi8(ms_vec, byte_idx);
        byte_idx = _mm256_add_epi16(byte_idx, _mm256_set1_epi16(0x0101));
        let mut d1 = _mm256_shuffle_epi8(ms_vec, byte_idx);

        // shift samples values to correct location
        let mut bit_shift = _mm256_shuffle_epi8(
            _mm256_set_epi8(
                1, 3, 7, 15, 31, 63, 127, -1, 1, 3, 7, 15, 31, 63, 127, -1, 1, 3, 7, 15, 31, 63,
                127, -1, 1, 3, 7, 15, 31, 63, 127, -1,
            ),
            bit_idx,
        );
        bit_shift = _mm256_add_epi16(bit_shift, _mm256_set1_epi16(0x0101));
        d0 = _mm256_mullo_epi16(d0, bit_shift);
        d0 = _mm256_srli_epi16::<8>(d0); // we should have 8 bits in the LSB
        d1 = _mm256_mullo_epi16(d1, bit_shift);
        d1 = _mm256_and_si256(d1, _mm256_set1_epi16(0xFF00_u16 as i16)); // 8 in MSB
        d0 = _mm256_or_si256(d0, d1);

        // find location of e_k and mask
        let ones = _mm256_set1_epi16(1);
        let twos = _mm256_set1_epi16(2);
        // shift = (2 - e_k) << (U_q - 1), through a pshufb power-of-two lookup and a 16-bit
        // multiply (for U_q == 0 the lookup gives 0)
        let kq = _mm256_sub_epi16(u_q_avx, ones);
        let idx = _mm256_or_si256(
            kq,
            _mm256_slli_epi16::<8>(_mm256_sub_epi16(kq, _mm256_set1_epi16(8))),
        );
        let pow2_tbl = _mm256_setr_epi8(
            1,
            2,
            4,
            8,
            16,
            32,
            64,
            128_u8 as i8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            1,
            2,
            4,
            8,
            16,
            32,
            64,
            128_u8 as i8,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        );
        let pow2 = _mm256_shuffle_epi8(pow2_tbl, idx);
        w0 = _mm256_sub_epi16(twos, w0);
        let shift = _mm256_mullo_epi16(w0, pow2);
        ms_vec = _mm256_and_si256(d0, _mm256_sub_epi16(shift, ones));

        // next e_1
        w0 = _mm256_and_si256(flags, _mm256_set1_epi16(0x800));
        w0 = _mm256_cmpeq_epi16(w0, _mm256_setzero_si256());
        w0 = _mm256_andnot_si256(w0, shift); // e_1 in correct position
        ms_vec = _mm256_or_si256(ms_vec, w0); // e_1
        w0 = _mm256_slli_epi16::<15>(ms_vec); // sign
        ms_vec = _mm256_or_si256(ms_vec, ones); // bin center
        let tvn = ms_vec;
        ms_vec = _mm256_add_epi16(ms_vec, twos); // + 2
        ms_vec = _mm256_sll_epi16(ms_vec, _mm_cvtsi32_si128(p as i32 - 1));
        ms_vec = _mm256_or_si256(ms_vec, w0); // sign
        row = _mm256_andnot_si256(insig, ms_vec); // significant only

        ms_vec = _mm256_andnot_si256(insig, tvn); // significant only

        let ms_vec_shuffle1 = _mm256_shuffle_epi8(
            ms_vec,
            _mm256_set_epi16(
                -1, -1, -1, -1, 0x0706, 0x0302, -1, -1, -1, -1, -1, -1, -1, -1, 0x0706, 0x0302,
            ),
        );
        let ms_vec_shuffle2 = _mm256_shuffle_epi8(
            ms_vec,
            _mm256_set_epi16(
                -1, -1, -1, 0x0F0E, 0x0B0A, -1, -1, -1, -1, -1, -1, -1, -1, 0x0F0E, 0x0B0A, -1,
            ),
        );
        ms_vec = _mm256_or_si256(ms_vec_shuffle1, ms_vec_shuffle2);

        *vn = _mm_or_si128(*vn, _mm256_castsi256_si128(ms_vec));
        *vn = _mm_or_si128(*vn, _mm256_extracti128_si256::<1>(ms_vec));
    }
    row
}

/// Port of `avx2_lzcnt_epi32`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:870-880 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
#[inline]
fn avx2_lzcnt_epi32(v: __m256i) -> __m256i {
    // prevent value from being rounded up to the next power of two
    let mut v = _mm256_andnot_si256(_mm256_srli_epi32::<8>(v), v); // keep 8 MSB

    v = _mm256_castps_si256(_mm256_cvtepi32_ps(v)); // convert an integer to float
    v = _mm256_srli_epi32::<23>(v); // shift down the exponent
    v = _mm256_subs_epu16(_mm256_set1_epi32(158), v); // undo bias
    v = _mm256_min_epi16(v, _mm256_set1_epi32(32)); // clamp at 32
    v
}

/// The bytes of the 16-bit MagSgn destuffing buffer (`dbuf_cap + 72`).
const DBUF16: usize = 4096 * 15 / 8 + 72;
/// The bytes of the 32-bit MagSgn destuffing buffer.
const DBUF32: usize = 4096 * 32 / 8 + 72;
/// `v_n_size`.
const V_N_SIZE: usize = 512 + 16;

/// Port of `decode_cb_step2_16bit`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:908-1030 @ OpenJPH 0.32.0).
///
/// # Safety
/// As [`ojph_decode_codeblock_avx2`]'s checks guarantee: `scratch` holds the quad rows read,
/// `decoded_data` every sample stored (8 at a time, two rows at a time), `coded_data` the
/// MagSgn segment.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn decode_cb_step2_16bit(
    scratch: *mut u16,
    decoded_data: *mut u32,
    coded_data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
    sstr: u32,
    p: u32,
    mmsbp2: u32,
    lcup: i32,
    scup: i32,
) -> bool {
    // reduce bitplane by 16 because we now have 16 bits instead of 32
    let p = p - 16;

    // (upstream zeroes 8 entries past width / 2 + 4, and reads nothing outside what it
    // writes or zeroes: the whole arrays are zeroed here, the same values where read)
    let mut v_n_scratch = [0u16; V_N_SIZE];
    let mut v_n_scratch_32 = [0u32; V_N_SIZE];

    // maximum consumable MagSgn bits: 4096 samples x (mmsbp2 < 16) bits
    let dbuf_cap: u32 = 4096 * 15 / 8;
    let mut dbuf = std::mem::MaybeUninit::<[u8; DBUF16]>::uninit();
    let dbuf = dbuf.as_mut_ptr() as *mut u8;
    // SAFETY (whole function): see the function's contract; the de-stuffing buffer holds
    // dbuf_cap + 72 bytes, the stack arrays V_N_SIZE entries (indices stay below
    // width / 2 + 16 <= 528).
    unsafe {
        let limit = destuff_frwd::<0xFF>(coded_data, lcup - scup, dbuf, dbuf_cap);
        let mut pos: u32 = 0;
        let stride = stride as usize;
        let sstr = sstr as usize;

        {
            let mut sp = scratch;
            let mut vp = v_n_scratch.as_mut_ptr();
            let mut dp = decoded_data;
            *vp = 2; // for easy calculation of emax

            let mut x = 0;
            while x < width {
                // process four quads
                let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                let u_q = _mm_srli_epi32::<16>(inf_u_q);
                let w = _mm_cmpgt_epi32(u_q, _mm_set1_epi32(mmsbp2 as i32));
                if _mm_testz_si128(w, w) == 0 {
                    return false;
                }

                let mut vn = _mm_set1_epi16(2);
                let row = decode_four_quad16(inf_u_q, u_q, dbuf, limit, &mut pos, p, &mut vn);

                let w = _mm_cvtsi32_si128(i32::from(*vp));
                _mm_storeu_si128(vp as *mut __m128i, _mm_or_si128(w, vn));

                let w0 = _mm256_shuffle_epi8(
                    row,
                    _mm256_set_epi16(
                        0x0D0C, -1, 0x0908, -1, 0x0504, -1, 0x0100, -1, 0x0D0C, -1, 0x0908, -1,
                        0x0504, -1, 0x0100, -1,
                    ),
                );
                let w1 = _mm256_shuffle_epi8(
                    row,
                    _mm256_set_epi16(
                        0x0F0E, -1, 0x0B0A, -1, 0x0706, -1, 0x0302, -1, 0x0F0E, -1, 0x0B0A, -1,
                        0x0706, -1, 0x0302, -1,
                    ),
                );

                _mm256_storeu_si256(dp as *mut __m256i, w0);
                _mm256_storeu_si256(dp.add(stride) as *mut __m256i, w1);
                x += 8;
                sp = sp.add(8);
                vp = vp.add(4);
                dp = dp.add(8);
            }
        }

        let mut y = 2;
        while y < height {
            {
                // perform 15 - count_leading_zeros(*vp) here
                let mut vp = v_n_scratch.as_ptr();
                let mut vp_32 = v_n_scratch_32.as_mut_ptr();

                let mut sp = scratch.add((y as usize >> 1) * sstr);
                let avx_mmsbp2 = _mm256_set1_epi32(mmsbp2 as i32);
                let avx_31 = _mm256_set1_epi32(31);
                let avx_f0 = _mm256_set1_epi32(0xF0);
                let avx_1 = _mm256_set1_epi32(1);
                let avx_0 = _mm256_setzero_si256();

                let mut x = 0;
                while x <= width {
                    let mut v = _mm_loadu_si128(vp as *const __m128i);
                    let v_p1 = _mm_loadu_si128(vp.add(1) as *const __m128i);
                    v = _mm_or_si128(v, v_p1);

                    let mut v_avx = _mm256_cvtepu16_epi32(v);
                    v_avx = avx2_lzcnt_epi32(v_avx);
                    v_avx = _mm256_sub_epi32(avx_31, v_avx);

                    let mut inf_u_q = _mm256_loadu_si256(sp as *const __m256i);
                    let mut gamma = _mm256_and_si256(inf_u_q, avx_f0);
                    let mut w0 = _mm256_sub_epi32(gamma, avx_1);
                    gamma = _mm256_and_si256(gamma, w0);
                    gamma = _mm256_cmpeq_epi32(gamma, avx_0);

                    v_avx = _mm256_andnot_si256(gamma, v_avx);
                    v_avx = _mm256_max_epi32(v_avx, avx_1);

                    inf_u_q = _mm256_srli_epi32::<16>(inf_u_q);
                    v_avx = _mm256_add_epi32(inf_u_q, v_avx);

                    w0 = _mm256_cmpgt_epi32(v_avx, avx_mmsbp2);
                    if _mm256_testz_si256(w0, w0) == 0 {
                        return false;
                    }

                    _mm256_storeu_si256(vp_32 as *mut __m256i, v_avx);
                    x += 16;
                    vp = vp.add(8);
                    sp = sp.add(16);
                    vp_32 = vp_32.add(8);
                }
            }

            let mut vp = v_n_scratch.as_mut_ptr();
            let mut vp_32 = v_n_scratch_32.as_ptr();
            let mut sp = scratch.add((y as usize >> 1) * sstr);
            let mut dp = decoded_data.add(y as usize * stride);
            *vp = 2; // for easy calculation of emax

            let mut x = 0;
            while x < width {
                // process four quads
                let inf_u_q = _mm_loadu_si128(sp as *const __m128i);
                let u_q = _mm_loadu_si128(vp_32 as *const __m128i);

                let mut vn = _mm_set1_epi16(2);
                let row = decode_four_quad16(inf_u_q, u_q, dbuf, limit, &mut pos, p, &mut vn);

                let w = _mm_cvtsi32_si128(i32::from(*vp));
                _mm_storeu_si128(vp as *mut __m128i, _mm_or_si128(w, vn));

                let w0 = _mm256_shuffle_epi8(
                    row,
                    _mm256_set_epi16(
                        0x0D0C, -1, 0x0908, -1, 0x0504, -1, 0x0100, -1, 0x0D0C, -1, 0x0908, -1,
                        0x0504, -1, 0x0100, -1,
                    ),
                );
                let w1 = _mm256_shuffle_epi8(
                    row,
                    _mm256_set_epi16(
                        0x0F0E, -1, 0x0B0A, -1, 0x0706, -1, 0x0302, -1, 0x0F0E, -1, 0x0B0A, -1,
                        0x0706, -1, 0x0302, -1,
                    ),
                );

                _mm256_storeu_si256(dp as *mut __m256i, w0);
                _mm256_storeu_si256(dp.add(stride) as *mut __m256i, w1);
                x += 8;
                sp = sp.add(8);
                vp = vp.add(4);
                dp = dp.add(8);
                vp_32 = vp_32.add(4);
            }
            y += 2;
        }
    }
    true
}

/// Port of `decode_cb_step2_32bit`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:1040-1158 @ OpenJPH 0.32.0).
///
/// # Safety
/// As [`decode_cb_step2_16bit`].
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn decode_cb_step2_32bit(
    scratch: *mut u16,
    decoded_data: *mut u32,
    coded_data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
    sstr: u32,
    p: u32,
    mmsbp2: u32,
    lcup: i32,
    scup: i32,
) -> bool {
    // (upstream zeroes 14 entries past width / 2 + 2; zeroed here whole, the same values
    // where read)
    let mut v_n_scratch = [0u32; 2 * V_N_SIZE];

    // maximum consumable MagSgn bits: 4096 samples x (mmsbp2 <= 32) bits
    let dbuf_cap: u32 = 4096 * 32 / 8;
    let mut dbuf = std::mem::MaybeUninit::<[u8; DBUF32]>::uninit();
    let dbuf = dbuf.as_mut_ptr() as *mut u8;
    // SAFETY (whole function): see the function's contract.
    unsafe {
        let limit = destuff_frwd::<0xFF>(coded_data, lcup - scup, dbuf, dbuf_cap);
        let mut pos: u32 = 0;
        let stride = stride as usize;
        let sstr = sstr as usize;

        let avx_mmsbp2 = _mm256_set1_epi32(mmsbp2 as i32);

        {
            let mut sp = scratch;
            let mut vp = v_n_scratch.as_mut_ptr();
            let mut dp = decoded_data;
            *vp = 2; // for easy calculation of emax

            let mut x = 0;
            while x < width {
                let mut vn = _mm_set1_epi32(2);

                let mut inf_u_q = _mm256_castsi128_si256(_mm_loadl_epi64(sp as *const __m128i));
                inf_u_q =
                    _mm256_permutevar8x32_epi32(inf_u_q, _mm256_setr_epi32(0, 0, 0, 0, 1, 1, 1, 1));

                let u_q = _mm256_srli_epi32::<16>(inf_u_q);
                let w = _mm256_cmpgt_epi32(u_q, avx_mmsbp2);
                if _mm256_testz_si256(w, w) == 0 {
                    return false;
                }

                let mut row =
                    decode_two_quad32_avx2(inf_u_q, u_q, dbuf, limit, &mut pos, p, &mut vn);
                row = _mm256_permutevar8x32_epi32(row, _mm256_setr_epi32(0, 2, 4, 6, 1, 3, 5, 7));
                _mm_storeu_si128(dp as *mut __m128i, _mm256_castsi256_si128(row));
                _mm_storeu_si128(
                    dp.add(stride) as *mut __m128i,
                    _mm256_extracti128_si256::<1>(row),
                );

                let mut w0 = _mm_cvtsi32_si128(*vp as i32);
                w0 = _mm_or_si128(w0, vn);
                _mm_storeu_si128(vp as *mut __m128i, w0);
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
                let mut sp = scratch.add((y as usize >> 1) * sstr);

                let avx_31 = _mm256_set1_epi32(31);
                let avx_f0 = _mm256_set1_epi32(0xF0);
                let avx_1 = _mm256_set1_epi32(1);
                let avx_0 = _mm256_setzero_si256();

                let mut x = 0;
                while x <= width {
                    let mut v = _mm256_loadu_si256(vp as *const __m256i);
                    let v_p1 = _mm256_loadu_si256(vp.add(1) as *const __m256i);
                    v = _mm256_or_si256(v, v_p1);
                    v = avx2_lzcnt_epi32(v);
                    v = _mm256_sub_epi32(avx_31, v);

                    let mut inf_u_q = _mm256_loadu_si256(sp as *const __m256i);
                    let mut gamma = _mm256_and_si256(inf_u_q, avx_f0);
                    let mut w0 = _mm256_sub_epi32(gamma, avx_1);
                    gamma = _mm256_and_si256(gamma, w0);
                    gamma = _mm256_cmpeq_epi32(gamma, avx_0);

                    v = _mm256_andnot_si256(gamma, v);
                    v = _mm256_max_epi32(v, avx_1);

                    inf_u_q = _mm256_srli_epi32::<16>(inf_u_q);
                    v = _mm256_add_epi32(inf_u_q, v);

                    w0 = _mm256_cmpgt_epi32(v, avx_mmsbp2);
                    if _mm256_testz_si256(w0, w0) == 0 {
                        return false;
                    }

                    _mm256_storeu_si256(vp.add(V_N_SIZE) as *mut __m256i, v);
                    x += 16;
                    vp = vp.add(8);
                    sp = sp.add(16);
                }
            }

            let mut vp = v_n_scratch.as_mut_ptr();
            let mut sp = scratch.add((y as usize >> 1) * sstr);
            let mut dp = decoded_data.add(y as usize * stride);
            *vp = 2; // for easy calculation of emax

            let mut x = 0;
            while x < width {
                // process two quads
                let mut vn = _mm_set1_epi32(2);

                let mut inf_u_q = _mm256_castsi128_si256(_mm_loadl_epi64(sp as *const __m128i));
                inf_u_q =
                    _mm256_permutevar8x32_epi32(inf_u_q, _mm256_setr_epi32(0, 0, 0, 0, 1, 1, 1, 1));

                let mut u_q =
                    _mm256_castsi128_si256(_mm_loadl_epi64(vp.add(V_N_SIZE) as *const __m128i));
                u_q = _mm256_permutevar8x32_epi32(u_q, _mm256_setr_epi32(0, 0, 0, 0, 1, 1, 1, 1));

                let mut row =
                    decode_two_quad32_avx2(inf_u_q, u_q, dbuf, limit, &mut pos, p, &mut vn);
                row = _mm256_permutevar8x32_epi32(row, _mm256_setr_epi32(0, 2, 4, 6, 1, 3, 5, 7));
                _mm_storeu_si128(dp as *mut __m128i, _mm256_castsi256_si128(row));
                _mm_storeu_si128(
                    dp.add(stride) as *mut __m128i,
                    _mm256_extracti128_si256::<1>(row),
                );

                let mut w0 = _mm_cvtsi32_si128(*vp as i32);
                w0 = _mm_or_si128(w0, vn);
                _mm_storeu_si128(vp as *mut __m128i, w0);
                x += 4;
                sp = sp.add(4);
                vp = vp.add(2);
                dp = dp.add(4);
            }
            y += 2;
        }
    }
    true
}

/// Spreads the 16 bits of `bits` into the 16 bytes of a vector, 0xFF where set (the idiom of
/// upstream's SPP and MRP: broadcast, select each byte's bit, compare).
#[target_feature(enable = "avx2")]
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

/// Port of `decode_cb_spp_mrp`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:1167-1560 @ OpenJPH 0.32.0):
/// the significance propagation and magnitude refinement passes.
///
/// # Safety
/// As [`ojph_decode_codeblock_avx2`]'s checks guarantee: `decoded_data` holds rows of 4 up to
/// the height rounded up to 4, `coded_data` holds `lengths1 + lengths2` bytes.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn decode_cb_spp_mrp(
    scratch: *mut u16,
    decoded_data: *mut u32,
    coded_data: *const u8,
    width: u32,
    height: u32,
    stride: u32,
    sstr: u32,
    p: u32,
    num_passes: u32,
    lengths1: u32,
    lengths2: u32,
    stripe_causal: bool,
) {
    // SAFETY (whole function): see the function's contract; sigma reuses the scratch rows
    // upstream reuses.
    unsafe {
        let stride = stride as usize;
        let sstr = sstr as usize;
        // sigma holds all the significant samples, and it cannot be modified after it is set.
        // it will be used during the Magnitude Refinement Pass
        let sigma = scratch;

        let mut mstr = ((width + 3) >> 2) as usize; // divide by 4, since each ui16 contains 4 columns
        mstr = ((mstr + 2) + 7) & !7; // multiples of 8

        // We re-arrange quad significance, where each 4 consecutive bits represent one quad,
        // into column significance, where, each 4 consequtive bits represent one column of 4
        // rows
        {
            let mask_3 = _mm_set1_epi32(0x30);
            let mask_c = _mm_set1_epi32(0xC0);
            let shuffle_mask = _mm_set_epi32(-1, -1, -1, 0x0C080400);
            let mut y = 0;
            while y < height {
                let mut sp = scratch.add((y as usize >> 1) * sstr);
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

                    let t = _mm_extract_epi32::<0>(r) as u32;
                    (dp as *mut u32).write_unaligned(t);
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
            // This stores significance information of the previous 4 rows.
            let mut prev_row_sig = [0u16; 256 + 8]; // 528 Bytes

            // maximum consumable SPP bits: 4096 samples x 2 bits (one significance bit and
            // one sign bit per sample)
            const SPP_CAP: u32 = 4096 * 2 / 8;
            let mut spp_buf = std::mem::MaybeUninit::<[u8; SPP_CAP as usize + 72]>::uninit();
            let spp_buf = spp_buf.as_mut_ptr() as *mut u8;
            let spp_limit = destuff_frwd::<0>(
                coded_data.add(lengths1 as usize),
                lengths2 as i32,
                spp_buf,
                SPP_CAP,
            );
            let mut spp_pos: u32 = 0;

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

                // prev holds sign. info. for the previous quad, together with the rows on top
                // of it and below it.
                let mut prev: u32 = 0;
                let mut prev_sig = prev_row_sig.as_mut_ptr();
                let mut cur_sig = sigma.add((y as usize >> 2) * mstr);
                let mut dpp = decoded_data.add(y as usize * stride);
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
                        let mut cwd = dfetch64(spp_buf, spp_limit, spp_pos);

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
                            // the sign bits sit right after the cnt consumed bits of cwd
                            let new_sig_vec = spread16(new_sig);

                            // find cumulative sums to find which bit in cwd we should extract
                            let mut inc_sum = new_sig_vec; // inclusive scan
                            inc_sum = _mm_abs_epi8(inc_sum); // cvrt to 0 or 1
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<1>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<2>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<4>(inc_sum));
                            inc_sum = _mm_add_epi8(inc_sum, _mm_bslli_si128::<8>(inc_sum));
                            cnt += (_mm_extract_epi16::<7>(inc_sum) as u32) >> 8;
                            // exclusive scan
                            let ex_sum = _mm_bslli_si128::<1>(inc_sum);

                            // Spread cwd, such that each bit is in one byte with a value of 0
                            // or 1.
                            let cwd_vec = _mm_abs_epi8(spread16(cwd as u32));

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
                                dp = dp.add(stride);
                                m = _mm_add_epi32(m, _mm_set1_epi32(1));
                            }
                        }
                        spp_pos += cnt;
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
            // de-stuff the MRP segment; consumption is at most 1 bit per sample = 512 bytes,
            // so 1024 bytes always suffice
            const MRP_CAP: u32 = 1024;
            let mut mrp_buf = std::mem::MaybeUninit::<[u8; MRP_CAP as usize + 72]>::uninit();
            let mrp_buf = mrp_buf.as_mut_ptr() as *mut u8;
            let mrp_limit = destuff_mrp(
                coded_data,
                lengths1 as i32,
                lengths2 as i32,
                mrp_buf,
                MRP_CAP,
            );
            let mut mrp_pos: u32 = 0;

            let mut y = 0;
            while y < height {
                let mut cur_sig = sigma.add((y as usize >> 2) * mstr);
                let mut dpp = decoded_data.add(y as usize * stride);
                let mut i = 0;
                while i < width {
                    // Process one entry from sigma array at a time
                    let cwd = dfetch64(mrp_buf, mrp_limit, mrp_pos);
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

                        // Spread the 16 bits in cwd to inverted 0 or 1 bytes in cwd_vec, then
                        // a value of 0 as binary 11, and a value of 1 as binary 01
                        let mut cwd_vec = spread16(cwd as u32);
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
                            dp = dp.add(stride);
                            m = _mm_add_epi32(m, _mm_set1_epi32(1));
                        }
                    }
                    // consume data according to the number of bits set
                    mrp_pos += total_bits as u32;
                    i += 4;
                    dpp = dpp.add(4);
                }
                y += 4;
            }
        }
    }
}

/// `scratch[i]` without a bounds check: step 1's indices stay inside the quad rows of a
/// codeblock of at most 4096 samples, as upstream's do (checked in debug builds).
#[inline(always)]
fn sget(scratch: &[u16], i: usize) -> u16 {
    debug_assert!(i < scratch.len());
    // SAFETY: see above
    unsafe { *scratch.get_unchecked(i) }
}

/// `scratch[i] = v` without a bounds check (see [`sget`]).
#[inline(always)]
fn sset(scratch: &mut [u16], i: usize, v: u16) {
    debug_assert!(i < scratch.len());
    // SAFETY: see sget
    unsafe { *scratch.get_unchecked_mut(i) = v }
}

/// A table entry without a bounds check: every index is masked below its table's size
/// (`c_q < 0x400 - 0x7F`, `uvlc_mode <= 0x100`, and so on, as upstream's).
#[inline(always)]
fn tget<T: Copy>(tbl: &[T], i: usize) -> T {
    debug_assert!(i < tbl.len());
    // SAFETY: see above
    unsafe { *tbl.get_unchecked(i) }
}

/// Port of `decode_cb_step1_vlc`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:1568-1817 @ OpenJPH 0.32.0):
/// the VLC and MEL segments decoded into `scratch`, two entries (inf, u_q) per quad.
///
/// # Safety
/// `coded_data` holds `lcup` bytes with `2 <= scup <= min(lcup, 4079)`.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn decode_cb_step1_vlc(
    scratch: &mut [u16],
    coded: &[u8],
    lcup: i32,
    scup: i32,
    width: u32,
    height: u32,
    sstr: u32,
) {
    let t = tables();
    // init structures
    let mut mel = DecMel::init(coded, lcup, scup);

    // de-stuff the VLC segment; its size is at most scup - 1 < 4095 bytes, and consumption per
    // quad pair is at most 7 + 7 + 30 bits, so 4096 bytes always suffice
    const VLC_CAP: u32 = 4096;
    let mut vlc_buf = std::mem::MaybeUninit::<[u8; VLC_CAP as usize + 72]>::uninit();
    let vlc_buf = vlc_buf.as_mut_ptr() as *mut u8;
    // SAFETY: the caller's contract; vlc_buf holds VLC_CAP + 72 bytes
    let vlc_limit = unsafe { destuff_vlc(coded.as_ptr(), lcup, scup, vlc_buf, VLC_CAP) };
    let mut vlc_off: u32 = 0;
    let mut vlc_bits: u32 = 0;

    let mut run = mel.get_run(); // decode runs of events from MEL bitstrm
    // data represented as runs of 0 events; See mel_decode description

    let mut vlc_val: u64 = 0;
    let mut c_q: u32;
    let sstr = sstr as usize;
    let mut sp = 0usize;
    // initial quad row
    c_q = 0;
    let mut x = 0u32;
    while x < width {
        // first quad
        // SAFETY: vlc_buf holds vlc_limit + 72 bytes
        unsafe {
            drefill(
                &mut vlc_val,
                &mut vlc_bits,
                &mut vlc_off,
                vlc_buf,
                vlc_limit,
            )
        };

        // decode VLC using the context c_q and the head of VLC bitstream
        let mut t0 = tget(&t.vlc_tbl0, (c_q + (vlc_val as u32 & 0x7F)) as usize);

        // if context is zero, use one MEL event
        if c_q == 0 {
            // zero context
            run -= 2; // subtract 2, since events number if multiplied by 2

            // Is the run terminated in 1? if so, use decoded VLC code, otherwise, discard
            // decoded data, since we will decoded again using a different context
            t0 = if run == -1 { t0 } else { 0 };

            // is run -1 or -2? this means a run has been consumed
            if run < 0 {
                run = mel.get_run(); // get another run
            }
        }
        sset(scratch, sp, t0);
        x += 2;

        // prepare context for the next quad; eqn. 1 in ITU T.814
        c_q = ((u32::from(t0) & 0x10) << 3) | ((u32::from(t0) & 0xE0) << 2);

        // remove data from vlc stream (0 bits are removed if vlc is not used)
        dconsume(&mut vlc_val, &mut vlc_bits, u32::from(t0) & 0x7);

        // second quad
        // decode VLC using the context c_q and the head of VLC bitstream
        let mut t1 = tget(&t.vlc_tbl0, (c_q + (vlc_val as u32 & 0x7F)) as usize);

        // if context is zero, use one MEL event
        if c_q == 0 && x < width {
            // zero context
            run -= 2; // subtract 2, since events number if multiplied by 2

            // if event is 0, discard decoded t1
            t1 = if run == -1 { t1 } else { 0 };

            if run < 0 {
                // have we consumed all events in a run
                run = mel.get_run(); // if yes, then get another run
            }
        }
        t1 = if x < width { t1 } else { 0 };
        sset(scratch, sp + 2, t1);
        x += 2;

        // prepare context for the next quad, eqn. 1 in ITU T.814
        c_q = ((u32::from(t1) & 0x10) << 3) | ((u32::from(t1) & 0xE0) << 2);

        // remove data from vlc stream, if qinf is not used, cwdlen is 0
        dconsume(&mut vlc_val, &mut vlc_bits, u32::from(t1) & 0x7);

        // decode u
        // uvlc_mode is made up of u_offset bits from the quad pair
        let mut uvlc_mode = ((u32::from(t0) & 0x8) << 3) | ((u32::from(t1) & 0x8) << 4);
        if uvlc_mode == 0xc0 {
            // if both u_offset are set, get an event from the MEL run of events
            run -= 2; // subtract 2, since events number if multiplied by 2

            uvlc_mode += if run == -1 { 0x40 } else { 0 }; // increment uvlc_mode by is 0x40

            if run < 0 {
                // if run is consumed (run is -1 or -2), get another run
                run = mel.get_run();
            }
        }

        // decode uvlc_mode to get u for both quads
        let mut uvlc_entry = u32::from(tget(
            &t.uvlc_tbl0,
            (uvlc_mode + (vlc_val as u32 & 0x3F)) as usize,
        ));
        // remove total prefix length
        dconsume(&mut vlc_val, &mut vlc_bits, uvlc_entry & 0x7);
        uvlc_entry >>= 3;
        // extract suffixes for quad 0 and 1
        let mut len = uvlc_entry & 0xF; // suffix length for 2 quads
        let tmp = (vlc_val as u32) & 1u32.wrapping_shl(len).wrapping_sub(1); // suffix value, 2 quads
        dconsume(&mut vlc_val, &mut vlc_bits, len);
        uvlc_entry >>= 4;
        // quad 0 length
        len = uvlc_entry & 0x7; // quad 0 suffix length
        uvlc_entry >>= 3;
        let mut u_q = (1 + (uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16; // kappa = 1
        sset(scratch, sp + 1, u_q);
        u_q = (1 + (uvlc_entry >> 3) + (tmp >> len)) as u16; // kappa == 1
        sset(scratch, sp + 3, u_q);
        sp += 4;
    }
    sset(scratch, sp, 0);
    sset(scratch, sp + 1, 0);

    // non initial quad rows
    let mut y = 2;
    while y < height {
        c_q = 0; // context
        let mut sp = (y as usize >> 1) * sstr; // this row of quads

        let mut x = 0u32;
        while x < width {
            // sigma_q (n, ne, nf)
            c_q |= (u32::from(sget(scratch, sp - sstr)) & 0xA0) << 2;
            c_q |= (u32::from(sget(scratch, sp + 2 - sstr)) & 0x20) << 4;

            // first quad
            // SAFETY: vlc_buf holds vlc_limit + 72 bytes
            unsafe {
                drefill(
                    &mut vlc_val,
                    &mut vlc_bits,
                    &mut vlc_off,
                    vlc_buf,
                    vlc_limit,
                )
            };

            // decode VLC using the context c_q and the head of VLC bitstream
            let mut t0 = tget(&t.vlc_tbl1, (c_q + (vlc_val as u32 & 0x7F)) as usize);

            // if context is zero, use one MEL event
            if c_q == 0 {
                // zero context
                run -= 2; // subtract 2, since events number is multiplied by 2

                // Is the run terminated in 1? if so, use decoded VLC code, otherwise, discard
                // decoded data, since we will decoded again using a different context
                t0 = if run == -1 { t0 } else { 0 };

                // is run -1 or -2? this means a run has been consumed
                if run < 0 {
                    run = mel.get_run(); // get another run
                }
            }
            sset(scratch, sp, t0);
            x += 2;

            // prepare context for the next quad; eqn. 2 in ITU T.814
            // sigma_q (w, sw)
            c_q = ((u32::from(t0) & 0x40) << 2) | ((u32::from(t0) & 0x80) << 1);
            // sigma_q (nw)
            c_q |= u32::from(sget(scratch, sp - sstr)) & 0x80;
            // sigma_q (n, ne, nf)
            c_q |= (u32::from(sget(scratch, sp + 2 - sstr)) & 0xA0) << 2;
            c_q |= (u32::from(sget(scratch, sp + 4 - sstr)) & 0x20) << 4;

            // remove data from vlc stream (0 bits are removed if vlc is unused)
            dconsume(&mut vlc_val, &mut vlc_bits, u32::from(t0) & 0x7);

            // second quad
            // decode VLC using the context c_q and the head of VLC bitstream
            let mut t1 = tget(&t.vlc_tbl1, (c_q + (vlc_val as u32 & 0x7F)) as usize);

            // if context is zero, use one MEL event
            if c_q == 0 && x < width {
                // zero context
                run -= 2; // subtract 2, since events number if multiplied by 2

                // if event is 0, discard decoded t1
                t1 = if run == -1 { t1 } else { 0 };

                if run < 0 {
                    // have we consumed all events in a run
                    run = mel.get_run(); // if yes, then get another run
                }
            }
            t1 = if x < width { t1 } else { 0 };
            sset(scratch, sp + 2, t1);
            x += 2;

            // partial c_q, will be completed when we process the next quad
            // sigma_q (w, sw)
            c_q = ((u32::from(t1) & 0x40) << 2) | ((u32::from(t1) & 0x80) << 1);
            // sigma_q (nw)
            c_q |= u32::from(sget(scratch, sp + 2 - sstr)) & 0x80;

            // remove data from vlc stream, if qinf is not used, cwdlen is 0
            dconsume(&mut vlc_val, &mut vlc_bits, u32::from(t1) & 0x7);

            // decode u using wide UVLC table
            let mut uvlc_mode = ((u32::from(t0) >> 3) & 1) | (((u32::from(t1) >> 3) & 1) << 1);
            let mut uvlc_entry = tget(
                &t.uvlc_tbl1_wide,
                ((uvlc_mode << 10) | (vlc_val as u32 & 0x3FF)) as usize,
            );
            let total_bits = uvlc_entry & 0x1F;
            if total_bits < 0x1F {
                sset(scratch, sp + 1, ((uvlc_entry >> 5) & 0xFF) as u16);
                sset(scratch, sp + 3, ((uvlc_entry >> 13) & 0xFF) as u16);
                dconsume(&mut vlc_val, &mut vlc_bits, total_bits);
            } else {
                uvlc_mode = ((u32::from(t0) & 0x8) << 3) | ((u32::from(t1) & 0x8) << 4);
                uvlc_entry = u32::from(tget(
                    &t.uvlc_tbl1,
                    (uvlc_mode + (vlc_val as u32 & 0x3F)) as usize,
                ));
                dconsume(&mut vlc_val, &mut vlc_bits, uvlc_entry & 0x7);
                uvlc_entry >>= 3;
                let mut len = uvlc_entry & 0xF;
                let tmp = (vlc_val as u32) & 1u32.wrapping_shl(len).wrapping_sub(1);
                dconsume(&mut vlc_val, &mut vlc_bits, len);
                uvlc_entry >>= 4;
                len = uvlc_entry & 0x7;
                uvlc_entry >>= 3;
                sset(
                    scratch,
                    sp + 1,
                    ((uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16,
                );
                sset(scratch, sp + 3, ((uvlc_entry >> 3) + (tmp >> len)) as u16);
            }
            sp += 4;
        }
        sset(scratch, sp, 0);
        sset(scratch, sp + 1, 0);
        y += 2;
    }
}

static INSUFFICIENT_PRECISION: AtomicBool = AtomicBool::new(false);
static MODIFY_CODE: AtomicBool = AtomicBool::new(false);
static TRUNCATE_SPP_MRP: AtomicBool = AtomicBool::new(false);

/// The scratch array's entries: upstream's `8 * 513`, and 32 more, which nothing reads that
/// upstream does not (the 32-bit path's loads of 16 entries end at most at upstream's end).
const SCRATCH: usize = 8 * 513 + 32;

thread_local! {
    /// The decoder's scratch array (see [`ojph_decode_codeblock_avx2`]).
    static SCRATCH_TLS: std::cell::RefCell<[u16; SCRATCH]> =
        const { std::cell::RefCell::new([0; SCRATCH]) };
}

/// Port of `ojph_decode_codeblock_avx2`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder_avx2.cpp:1819-1972 @ OpenJPH 0.32.0):
/// decodes one codeblock, the cleanup, significance propagation and magnitude refinement
/// passes. `None` when the buffers cannot hold every access upstream makes (the caller
/// then uses the generic decoder; the codeblock layer always gives enough).
///
/// # Safety
/// The CPU supports AVX2.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn ojph_decode_codeblock_avx2(
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
    // to 4, 8 samples a store below the stride (a multiple of 8, at least the width); the
    // coded data's two segments; widths and heights within a codeblock's
    if width == 0
        || height == 0
        || width > 1024
        || height > 1024
        || width * height > 4096
        || stride % 8 != 0
        || stride < ((width + 7) & !7)
        || (((height + 3) & !3) as usize) * (stride as usize) > decoded_data.len()
        || (lengths1 as usize) + (lengths2 as usize) > coded_data.len()
    {
        return None;
    }

    if num_passes > 1 && lengths2 == 0 {
        warn(
            0x00010001,
            FILE,
            1831,
            "A malformed codeblock that has more than one coding pass, but zero length for 2nd and potential 3rd pass.",
            &[],
        );
        num_passes = 1;
    }

    if num_passes > 3 {
        warn(
            0x00010002,
            FILE,
            1839,
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
                1850,
                "32 bits are not enough to decode this codeblock. This message will not be displayed again.",
                &[],
            );
        }
        return Some(false);
    } else if missing_msbs == 30 {
        // p == 0: not enough precision to decode and set the bin center to 1
        if !MODIFY_CODE.swap(true, Ordering::Relaxed) {
            warn(
                0x00010004,
                FILE,
                1860,
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
                    1873,
                    "Not enough precision to decode the SgnProp nor MagRef passes; both will be skipped. This message will not be displayed again.",
                    &[],
                );
            }
        }
    }
    let p = 30 - missing_msbs; // The least significant bitplane for CUP

    if lengths1 < 2 {
        warn(0x00010006, FILE, 1886, "Wrong codeblock length.", &[]);
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

    // We need an extra two entries (one inf and one u_q) beyond the last column.
    let sstr = ((width + 2) + 7) & !7; // multiples of 8

    // upstream's scratch is a stack array of which it zeroes the quad rows it uses and one
    // more; here a thread's array, with the same entries zeroed (nothing outside them is read)
    SCRATCH_TLS.with_borrow_mut(|scratch| {
        let quad_rows = ((height + 1) >> 1) as usize;
        let scratch_zero = ((quad_rows + 1) * sstr as usize).min(8 * 513);
        scratch[..scratch_zero].fill(0);
        // SAFETY: AVX2 (this function's), and the checks above
        unsafe {
            decode_with_scratch(
                scratch,
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
                p,
                lcup,
                scup,
                sstr,
            )
        }
    })
}

/// The rest of [`ojph_decode_codeblock_avx2`], with its scratch array.
///
/// # Safety
/// As [`ojph_decode_codeblock_avx2`], whose checks it follows.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn decode_with_scratch(
    scratch: &mut [u16; SCRATCH],
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
    p: u32,
    lcup: i32,
    scup: i32,
    sstr: u32,
) -> Option<bool> {
    let mmsbp2 = missing_msbs + 2;

    // step 1: decode VLC and MEL segments into scratch
    // SAFETY: AVX2, and the checks above
    unsafe {
        decode_cb_step1_vlc(scratch, coded_data, lcup, scup, width, height, sstr);

        // step2 we decode magsgn
        let ok = if mmsbp2 >= 16 {
            decode_cb_step2_32bit(
                scratch.as_mut_ptr(),
                decoded_data.as_mut_ptr(),
                coded_data.as_ptr(),
                width,
                height,
                stride,
                sstr,
                p,
                mmsbp2,
                lcup,
                scup,
            )
        } else {
            decode_cb_step2_16bit(
                scratch.as_mut_ptr(),
                decoded_data.as_mut_ptr(),
                coded_data.as_ptr(),
                width,
                height,
                stride,
                sstr,
                p,
                mmsbp2,
                lcup,
                scup,
            )
        };
        if !ok {
            return Some(false);
        }

        if num_passes > 1 {
            decode_cb_spp_mrp(
                scratch.as_mut_ptr(),
                decoded_data.as_mut_ptr(),
                coded_data.as_ptr(),
                width,
                height,
                stride,
                sstr,
                p,
                num_passes,
                lengths1,
                lengths2,
                stripe_causal,
            );
        }
    }
    Some(true)
}

/// [`ojph_decode_codeblock_avx2`] where the CPU has AVX2 (upstream's level check,
/// `get_cpu_ext_level() >= X86_CPU_EXT_LEVEL_AVX2`, implies it); `None` otherwise, or when the
/// buffers cannot hold its accesses.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_codeblock_avx2(
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
    if !std::arch::is_x86_feature_detected!("avx2") {
        return None;
    }
    // SAFETY: the CPU has AVX2
    unsafe {
        ojph_decode_codeblock_avx2(
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
