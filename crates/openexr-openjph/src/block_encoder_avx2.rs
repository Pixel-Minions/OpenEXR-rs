// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_encoder_avx2.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1-1355 @ OpenJPH 0.32.0):
//! the HT block encoder upstream chooses from `X86_CPU_EXT_LEVEL_AVX2` below AVX-512
//! (`codeblock_fun::init`; built with `-mavx2`, `/arch:AVX2` under MSVC), 16 samples of two
//! lines per step, with its own writers: MEL bits emitted several at a time, and VLC and
//! MagSgn bits gathered in 64-bit words and drained.
//!
//! Upstream's `mel_emit_bits`, `vlc_drain` and `ms_drain` write without checking their
//! buffers, which are "more than enough" for a codeblock's 4096 samples; here a write past
//! them is refused with the generic encoder's messages, which no codeblock reaches.

#![allow(unsafe_code)]

use std::arch::x86_64::*;
use std::sync::OnceLock;

use crate::arch::population_count;
use crate::bitbuffer_read::{PREFIX_BUF_SIZE, SUFFIX_BUF_SIZE};
use crate::block_common::{TBL0, TBL1};
use crate::message::{OjphResult, error};

const FILE: &str = "ojph_block_encoder_avx2.cpp";
/// The generic encoder's file, whose messages a write past a buffer gives.
const GEN_FILE: &str = "ojph_block_encoder.cpp";

/// The encoder's tables (`vlc_tbl0`, `vlc_tbl1`, `uvlc_tbl_pair1`, `uvlc_tbl_pair2`).
struct Tables {
    vlc_tbl0: [u32; 2048],
    vlc_tbl1: [u32; 2048],
    uvlc_tbl_pair1: [u32; 33 * 33],
    uvlc_tbl_pair2: [u32; 33 * 33],
}

/// Port of one table of `vlc_init_tables`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:92-209 @ OpenJPH 0.32.0).
fn vlc_init_table(tgt_tbl: &mut [u32; 2048], src_tbl: &[[u8; 7]]) {
    let mut pattern_popcnt = [0i32; 16];
    for (i, p) in pattern_popcnt.iter_mut().enumerate() {
        *p = population_count(i as u32) as i32;
    }
    for (i, tgt) in tgt_tbl.iter_mut().enumerate() {
        let i = i as i32;
        let (c_q, rho, emb) = (i >> 8, (i >> 4) & 0xF, i & 0xF);
        if (emb & rho) != emb || (rho == 0 && c_q == 0) {
            *tgt = 0;
            continue;
        }
        let mut best_entry: Option<&[u8; 7]> = None;
        if emb != 0 {
            // u_off = 1
            let mut best_e_k = -1;
            for e in src_tbl {
                let (e_c_q, e_rho, u_off, e_k, e_1) = (
                    i32::from(e[0]),
                    i32::from(e[1]),
                    e[2],
                    i32::from(e[3]),
                    i32::from(e[4]),
                );
                if e_c_q == c_q && e_rho == rho && u_off == 1 && (emb & e_k) == e_1 {
                    // the smallest cwd with the highest number of bits set in e_k
                    let ones_count = pattern_popcnt[e_k as usize];
                    if ones_count >= best_e_k {
                        best_entry = Some(e);
                        best_e_k = ones_count;
                    }
                }
            }
        } else {
            // u_off = 0
            best_entry = src_tbl
                .iter()
                .find(|e| i32::from(e[0]) == c_q && i32::from(e[1]) == rho && e[2] == 0);
        }
        let e = best_entry.expect("table0.h and table1.h cover every valid index");
        // (ui16) of (cwd << 8) + (cwd_len << 4) + e_k, as upstream stores it
        *tgt =
            u32::from((((u32::from(e[5]) << 8) + (u32::from(e[6]) << 4)) + u32::from(e[3])) as u16);
    }
}

/// Port of `initialize_block_encoder_tables_avx2`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:286-297 @ OpenJPH 0.32.0),
/// with `vlc_init_tables`, `uvlc_init_tables` and `uvlc_init_pair_tables` (lines 92-283).
fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            vlc_tbl0: [0; 2048],
            vlc_tbl1: [0; 2048],
            uvlc_tbl_pair1: [0; 33 * 33],
            uvlc_tbl_pair2: [0; 33 * 33],
        };
        vlc_init_table(&mut t.vlc_tbl0, &TBL0);
        vlc_init_table(&mut t.vlc_tbl1, &TBL1);
        // code goes from 0 to 31, extension and 32 are not supported here
        let mut pre = [0u32; 33];
        let mut pre_len = [0i32; 33];
        let mut suf = [0u32; 33];
        let mut suf_len = [0i32; 33];
        pre[..5].copy_from_slice(&[0, 1, 2, 4, 4]);
        pre_len[..5].copy_from_slice(&[0, 1, 2, 3, 3]);
        suf[..5].copy_from_slice(&[0, 0, 0, 0, 1]);
        suf_len[..5].copy_from_slice(&[0, 0, 0, 1, 1]);
        for i in 5..33 {
            pre[i] = 0;
            pre_len[i] = 3;
            suf[i] = (i - 5) as u32;
            suf_len[i] = 5;
        }
        // uvlc_init_pair_tables
        for uq0 in 0..33usize {
            for uq1 in 0..33usize {
                let (mut cwd, mut len) = (0u32, 0i32);
                if uq0 > 2 && uq1 > 2 {
                    cwd |= pre[uq0 - 2];
                    len += pre_len[uq0 - 2];
                    cwd |= pre[uq1 - 2] << len;
                    len += pre_len[uq1 - 2];
                    cwd |= suf[uq0 - 2] << len;
                    len += suf_len[uq0 - 2];
                    cwd |= suf[uq1 - 2] << len;
                    len += suf_len[uq1 - 2];
                } else if uq0 > 2 && uq1 > 0 {
                    cwd |= pre[uq0];
                    len += pre_len[uq0];
                    cwd |= ((uq1 - 1) as u32) << len;
                    len += 1;
                    cwd |= suf[uq0] << len;
                    len += suf_len[uq0];
                } else {
                    cwd |= pre[uq0];
                    len += pre_len[uq0];
                    cwd |= pre[uq1] << len;
                    len += pre_len[uq1];
                    cwd |= suf[uq0] << len;
                    len += suf_len[uq0];
                    cwd |= suf[uq1] << len;
                    len += suf_len[uq1];
                }
                t.uvlc_tbl_pair1[uq0 * 33 + uq1] = (cwd << 5) | len as u32;

                let (mut cwd, mut len) = (0u32, 0i32);
                cwd |= pre[uq0];
                len += pre_len[uq0];
                cwd |= pre[uq1] << len;
                len += pre_len[uq1];
                cwd |= suf[uq0] << len;
                len += suf_len[uq0];
                cwd |= suf[uq1] << len;
                len += suf_len[uq1];
                t.uvlc_tbl_pair2[uq0 * 33 + uq1] = (cwd << 5) | len as u32;
            }
        }
        t
    })
}

/// `mel_size` and `vlc_size`: the parts of upstream's `mel_vlc_buf` of 3072 bytes.
const MEL_SIZE: usize = 192;
const VLC_SIZE: usize = 3072 - MEL_SIZE;
/// `ms_size`.
const MS_SIZE: usize = (16384 * 16 + 14) / 15;

/// `mel_exp` (line 330).
const MEL_EXP: [i32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

/// Port of `mel_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:302-314
/// @ OpenJPH 0.32.0).
struct Mel {
    buf: [u8; MEL_SIZE],
    pos: u32,
    remaining_bits: i32,
    tmp: i32,
    run: i32,
    k: i32,
    threshold: i32,
}

impl Mel {
    /// Port of `mel_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:317-328
    /// @ OpenJPH 0.32.0).
    fn new() -> Self {
        Mel {
            buf: [0; MEL_SIZE],
            pos: 0,
            remaining_bits: 8,
            tmp: 0,
            run: 0,
            k: 0,
            threshold: 1, // this is 1 << mel_exp[melp->k];
        }
    }

    /// Port of `mel_emit_bits`
    /// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:333-345 @ OpenJPH
    /// 0.32.0), its buffer checked (see the module).
    #[inline]
    fn emit_bits(&mut self, bits: u32, num_bits: i32) -> OjphResult<()> {
        self.tmp = (self.tmp << num_bits) | bits as i32;
        self.remaining_bits -= num_bits;
        if self.remaining_bits <= 0 {
            let excess = -self.remaining_bits;
            let byte = (self.tmp >> excess) as u8;
            let Some(b) = self.buf.get_mut(self.pos as usize) else {
                return Err(error(
                    0x00020001,
                    GEN_FILE,
                    311,
                    "mel encoder's buffer is full",
                    &[],
                ));
            };
            *b = byte;
            self.pos += 1;
            self.tmp &= (1 << excess) - 1;
            self.remaining_bits += 8 - i32::from(byte == 0xFF);
        }
        Ok(())
    }

    /// Port of `mel_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:348-366
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn encode(&mut self, bit: bool) -> OjphResult<()> {
        if !bit {
            self.run += 1;
            if self.run >= self.threshold {
                self.emit_bits(1, 1)?;
                self.run = 0;
                self.k = if 12 < self.k + 1 { 12 } else { self.k + 1 };
                self.threshold = 1 << MEL_EXP[self.k as usize];
            }
        } else {
            let t = MEL_EXP[self.k as usize];
            self.emit_bits(self.run as u32 & ((1u32 << t) - 1), t + 1)?;
            self.run = 0;
            self.k = if 0 > self.k - 1 { 0 } else { self.k - 1 };
            self.threshold = 1 << MEL_EXP[self.k as usize];
        }
        Ok(())
    }
}

/// Port of `vlc_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:403-412
/// @ OpenJPH 0.32.0): written backward from the end of `buf`.
struct Vlc {
    buf: [u8; VLC_SIZE],
    pos: u32,
    used_bits: i32,
    tmp: u64,
    last_greater_than_8f: bool,
}

impl Vlc {
    /// Port of `vlc_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:415-426
    /// @ OpenJPH 0.32.0).
    fn new() -> Self {
        let mut buf = [0u8; VLC_SIZE];
        buf[VLC_SIZE - 1] = 0xFF;
        Vlc {
            buf,
            pos: 1, // locations will be all -pos
            used_bits: 4,
            tmp: 0xF,
            last_greater_than_8f: true,
        }
    }

    /// `*(vlcp->buf - vlcp->pos) = v`, its buffer checked (see the module).
    #[inline]
    fn put(&mut self, v: u8) -> OjphResult<()> {
        let Some(b) = (VLC_SIZE - 1)
            .checked_sub(self.pos as usize)
            .and_then(|i| self.buf.get_mut(i))
        else {
            return Err(error(
                0x00020002,
                GEN_FILE,
                384,
                "vlc encoder's buffer is full",
                &[],
            ));
        };
        *b = v;
        Ok(())
    }

    /// Port of `vlc_drain` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:429-445
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn drain(&mut self) -> OjphResult<()> {
        while self.used_bits >= 8 {
            let escape = i32::from(self.last_greater_than_8f);
            let is_7f = i32::from((self.tmp & 0x7F) == 0x7F);
            let need_stuff = if (escape & is_7f) != 0 { 1 } else { 0 };
            let bits = 8 - need_stuff;

            let byte = (self.tmp & ((1u64 << bits) - 1)) as u8;
            self.put(byte)?;
            self.pos += 1;
            self.tmp >>= bits;
            self.used_bits -= bits;
            self.last_greater_than_8f = byte > 0x8F;
        }
        Ok(())
    }

    /// Port of `vlc_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:448-465
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn encode(&mut self, mut cwd: u64, mut cwd_len: i32) -> OjphResult<()> {
        loop {
            let avail = 64 - self.used_bits;
            if avail > 0 && cwd_len <= avail {
                self.tmp |= cwd.wrapping_shl(self.used_bits as u32);
                self.used_bits += cwd_len;
                return Ok(());
            }
            if avail > 0 {
                // available space smaller than needed
                self.tmp |= cwd.wrapping_shl(self.used_bits as u32);
            }
            self.used_bits = 64;
            self.drain()?;
            cwd = cwd.wrapping_shr(avail as u32);
            cwd_len -= avail;
        }
    }
}

/// Port of `terminate_mel_vlc`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:470-507 @ OpenJPH 0.32.0).
fn terminate_mel_vlc(mel: &mut Mel, vlc: &mut Vlc) -> OjphResult<()> {
    if mel.run > 0 {
        mel.emit_bits(1, 1)?;
    }

    if vlc.last_greater_than_8f && (vlc.tmp & 0x7f) == 0x7f {
        vlc.put(0x7f)?;
        vlc.pos += 1;
        vlc.tmp >>= 7;
        vlc.used_bits -= 7;
    }

    mel.tmp <<= mel.remaining_bits;
    let mel_mask = (0xFF << mel.remaining_bits) & 0xFF;
    let vlc_mask = 0xFF >> (8 - vlc.used_bits);
    if (mel_mask | vlc_mask) == 0 {
        return Ok(()); // last mel byte cannot be 0xFF, since then remaining_bits would be < 8
    }
    if mel.pos as usize >= MEL_SIZE {
        return Err(error(
            0x00020003,
            FILE,
            490,
            "mel encoder's buffer is full",
            &[],
        ));
    }
    let vlcp_tmp = i32::from(vlc.tmp as u8);
    let fuse = mel.tmp | vlcp_tmp;
    if ((((fuse ^ mel.tmp) & mel_mask) | ((fuse ^ vlcp_tmp) & vlc_mask)) == 0)
        && fuse != 0xFF
        && vlc.pos > 1
    {
        mel.buf[mel.pos as usize] = fuse as u8;
        mel.pos += 1;
    } else {
        if vlc.pos as usize >= VLC_SIZE {
            return Err(error(
                0x00020004,
                FILE,
                502,
                "vlc encoder's buffer is full",
                &[],
            ));
        }
        mel.buf[mel.pos as usize] = mel.tmp as u8; // melp->tmp cannot be 0xFF
        mel.pos += 1;
        vlc.put(vlcp_tmp as u8)?;
        vlc.pos += 1;
    }
    Ok(())
}

/// Port of `ms_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:513-522
/// @ OpenJPH 0.32.0).
struct Ms {
    buf: crate::mem::Pooled<u8>,
    pos: u32,
    used_bits: i32,
    tmp: u64,
    last_was_ff: bool,
}

impl Ms {
    /// Port of `ms_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:525-534
    /// @ OpenJPH 0.32.0).
    fn new() -> Self {
        Ms {
            buf: crate::mem::Pooled::zeroed(MS_SIZE),
            pos: 0,
            used_bits: 0,
            tmp: 0,
            last_was_ff: false,
        }
    }

    /// The `n` bytes of `word` (little-endian) at `pos`, the buffer checked (see the module).
    #[inline]
    fn put(&mut self, word: u64, n: usize) -> OjphResult<()> {
        let at = self.pos as usize;
        let Some(dst) = self.buf.get_mut(at..at + n) else {
            return Err(error(
                0x00020005,
                GEN_FILE,
                476,
                "magnitude sign encoder's buffer is full",
                &[],
            ));
        };
        dst.copy_from_slice(&word.to_le_bytes()[..n]);
        self.pos += n as u32;
        Ok(())
    }

    /// Port of `ms_drain` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:537-593
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn drain(&mut self) -> OjphResult<()> {
        if self.last_was_ff {
            if self.used_bits < 7 {
                return Ok(());
            }
            self.put(self.tmp & 0x7F, 1)?;
            self.tmp >>= 7;
            self.used_bits -= 7;
            self.last_was_ff = false;
        }

        while self.used_bits >= 8 {
            let mut n_bytes = self.used_bits >> 3;
            if n_bytes > 8 {
                n_bytes = 8;
            }

            let word = self.tmp;
            let valid_mask = if n_bytes < 8 {
                (1u64 << (n_bytes * 8)) - 1
            } else {
                !0u64
            };

            let w = !word;
            let mut ff_detect = w.wrapping_sub(0x0101010101010101) & !w & 0x8080808080808080;
            ff_detect &= valid_mask;

            if ff_detect == 0 {
                self.put(word, n_bytes as usize)?;
                if n_bytes < 8 {
                    self.tmp >>= n_bytes * 8;
                } else {
                    self.tmp = 0;
                }
                self.used_bits -= n_bytes * 8;
            } else {
                let ff_pos = (ff_detect.trailing_zeros() >> 3) as i32;
                let safe = ff_pos + 1;
                self.put(word, safe as usize)?;
                let bits = safe * 8;
                if bits < 64 {
                    self.tmp >>= bits;
                } else {
                    self.tmp = 0;
                }
                self.used_bits -= bits;

                if self.used_bits >= 7 {
                    self.put(self.tmp & 0x7F, 1)?;
                    self.tmp >>= 7;
                    self.used_bits -= 7;
                    self.last_was_ff = false;
                } else {
                    self.last_was_ff = true;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// Port of `ms_encode_nodefer`
    /// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:596-613 @ OpenJPH 0.32.0).
    #[inline]
    fn encode_nodefer(&mut self, mut cwd: u64, mut cwd_len: i32) -> OjphResult<()> {
        loop {
            let avail = 64 - self.used_bits;
            if avail > 0 && cwd_len <= avail {
                self.tmp |= cwd.wrapping_shl(self.used_bits as u32);
                self.used_bits += cwd_len;
                return Ok(());
            }
            if avail > 0 {
                // available space smaller than needed
                self.tmp |= cwd.wrapping_shl(self.used_bits as u32);
            }
            self.used_bits = 64;
            self.drain()?;
            cwd = cwd.wrapping_shr(avail as u32);
            cwd_len -= avail;
        }
    }

    /// Port of `ms_terminate`
    /// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:636-655 @ OpenJPH 0.32.0).
    fn terminate(&mut self) -> OjphResult<()> {
        self.drain()?;
        if self.used_bits != 0 {
            let max_bits = if self.last_was_ff { 7 } else { 8 };
            let t = max_bits - self.used_bits;
            let mut byte = (self.tmp & ((1u64 << self.used_bits) - 1)) as u32;
            byte |= (0xFFu32 & ((1u32 << t) - 1)) << self.used_bits;
            if byte != 0xFF {
                if self.pos as usize >= MS_SIZE {
                    return Err(error(
                        0x00020006,
                        FILE,
                        649,
                        "magnitude sign encoder's buffer is full",
                        &[],
                    ));
                }
                self.buf[self.pos as usize] = byte as u8;
                self.pos += 1;
            }
        } else if self.last_was_ff {
            self.pos -= 1;
        }
        Ok(())
    }
}

/// Port of `avx2_lzcnt_epi32`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:661-671 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
#[inline]
fn avx2_lzcnt_epi32(v: __m256i) -> __m256i {
    // prevent value from being rounded up to the next power of two
    let mut v = _mm256_andnot_si256(_mm256_srli_epi32::<8>(v), v); // keep 8 MSB
    v = _mm256_castps_si256(_mm256_cvtepi32_ps(v)); // convert an integer to float
    v = _mm256_srli_epi32::<23>(v); // shift down the exponent
    v = _mm256_subs_epu16(_mm256_set1_epi32(158), v); // undo bias
    _mm256_min_epi16(v, _mm256_set1_epi32(32)) // clamp at 32
}

/// Port of `avx2_cmpneq_epi32`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:673-675 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
#[inline]
fn avx2_cmpneq_epi32(v: __m256i, v2: __m256i) -> __m256i {
    _mm256_xor_si256(_mm256_cmpeq_epi32(v, v2), _mm256_set1_epi32(-1))
}

/// Port of `proc_pixel` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:677-763
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_pixel(
    src_vec: &[__m256i; 4],
    p: u32,
    eq_vec: &mut [__m256i; 4],
    s_vec: &mut [__m256i; 4],
    rho_vec: &mut __m256i,
    e_qmax_vec: &mut __m256i,
) {
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let mut val_vec = [zero; 4];
    let mut eq = [zero; 4];
    let mut s = [zero; 4];
    let mut rho = [zero; 4];
    let count = _mm_cvtsi32_si128(p as i32);

    for i in 0..4 {
        // val = t + t; //multiply by 2 and get rid of sign
        val_vec[i] = _mm256_add_epi32(src_vec[i], src_vec[i]);
        // val >>= p;  // 2 \mu_p + x
        val_vec[i] = _mm256_srl_epi32(val_vec[i], count);
        // val &= ~1u; // 2 \mu_p
        val_vec[i] = _mm256_and_si256(val_vec[i], _mm256_set1_epi32(!1u32 as i32));
        // if (val) {
        let val_notmask = avx2_cmpneq_epi32(val_vec[i], zero);
        // e_q[i] = 32 - (int)count_leading_ZEROs(--val); //2\mu_p - 1
        val_vec[i] = _mm256_sub_epi32(val_vec[i], one);
        eq[i] = avx2_lzcnt_epi32(val_vec[i]);
        eq[i] = _mm256_sub_epi32(_mm256_set1_epi32(32), eq[i]);
        // s[0] = --val + (t >> 31); //v_n = 2(\mu_p-1) + s_n
        val_vec[i] = _mm256_sub_epi32(val_vec[i], one);
        s[i] = _mm256_srli_epi32::<31>(src_vec[i]);
        s[i] = _mm256_add_epi32(s[i], val_vec[i]);

        eq[i] = _mm256_and_si256(eq[i], val_notmask);
        s[i] = _mm256_and_si256(s[i], val_notmask);
        val_vec[i] = _mm256_srli_epi32::<31>(val_notmask);
        // }
    }

    let idx = _mm256_set_epi32(7, 5, 3, 1, 6, 4, 2, 0);

    // reorder to [0, 0], [0, 2] ... | [1, 0], [1, 2] ... | [0, 1] ... | [1, 1] ...
    for i in 0..2 {
        let tmp1 = _mm256_permutevar8x32_epi32(eq[i], idx);
        let tmp2 = _mm256_permutevar8x32_epi32(eq[2 + i], idx);
        eq_vec[i] = _mm256_permute2x128_si256::<0x20>(tmp1, tmp2);
        eq_vec[2 + i] = _mm256_permute2x128_si256::<0x31>(tmp1, tmp2);

        let tmp1 = _mm256_permutevar8x32_epi32(s[i], idx);
        let tmp2 = _mm256_permutevar8x32_epi32(s[2 + i], idx);
        s_vec[i] = _mm256_permute2x128_si256::<0x20>(tmp1, tmp2);
        s_vec[2 + i] = _mm256_permute2x128_si256::<0x31>(tmp1, tmp2);

        let tmp1 = _mm256_permutevar8x32_epi32(val_vec[i], idx);
        let tmp2 = _mm256_permutevar8x32_epi32(val_vec[2 + i], idx);
        rho[i] = _mm256_permute2x128_si256::<0x20>(tmp1, tmp2);
        rho[2 + i] = _mm256_permute2x128_si256::<0x31>(tmp1, tmp2);
    }

    *e_qmax_vec = _mm256_max_epi32(eq_vec[0], eq_vec[1]);
    *e_qmax_vec = _mm256_max_epi32(*e_qmax_vec, eq_vec[2]);
    *e_qmax_vec = _mm256_max_epi32(*e_qmax_vec, eq_vec[3]);
    rho[1] = _mm256_slli_epi32::<1>(rho[1]);
    rho[2] = _mm256_slli_epi32::<2>(rho[2]);
    rho[3] = _mm256_slli_epi32::<3>(rho[3]);
    *rho_vec = _mm256_or_si256(rho[0], rho[1]);
    *rho_vec = _mm256_or_si256(*rho_vec, rho[2]);
    *rho_vec = _mm256_or_si256(*rho_vec, rho[3]);
}

/// Port of `rotate_matrix`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:778-797 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn rotate_matrix(matrix: &mut [__m256i; 4]) {
    let mut tmp1 = _mm256_unpacklo_epi32(matrix[0], matrix[1]);
    let tmp2 = _mm256_unpacklo_epi32(matrix[2], matrix[3]);
    let tmp3 = _mm256_unpackhi_epi32(matrix[0], matrix[1]);
    let tmp4 = _mm256_unpackhi_epi32(matrix[2], matrix[3]);

    matrix[0] = _mm256_unpacklo_epi64(tmp1, tmp2);
    matrix[1] = _mm256_unpacklo_epi64(tmp3, tmp4);
    matrix[2] = _mm256_unpackhi_epi64(tmp1, tmp2);
    matrix[3] = _mm256_unpackhi_epi64(tmp3, tmp4);

    tmp1 = _mm256_permute2x128_si256::<0x20>(matrix[0], matrix[2]);
    matrix[2] = _mm256_permute2x128_si256::<0x31>(matrix[0], matrix[2]);
    matrix[0] = tmp1;

    tmp1 = _mm256_permute2x128_si256::<0x20>(matrix[1], matrix[3]);
    matrix[3] = _mm256_permute2x128_si256::<0x31>(matrix[1], matrix[3]);
    matrix[1] = tmp1;
}

/// The 8 lanes of `v`.
#[target_feature(enable = "avx2")]
#[inline]
fn lanes(v: __m256i) -> [u32; 8] {
    let mut out = [0u32; 8];
    // SAFETY: out holds 8 u32
    unsafe { _mm256_storeu_si256(out.as_mut_ptr() as *mut __m256i, v) };
    out
}

/// Port of `proc_ms_encode`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:799-882 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_ms_encode(
    msp: &mut Ms,
    tuple_vec: __m256i,
    uq_vec: __m256i,
    rho_vec: __m256i,
    s_vec: &mut [__m256i; 4],
) -> OjphResult<()> {
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let mut m_vec = [zero; 4];

    // m = (rho[i] & (1 << n)) ? Uq[i] - ((tuple[i] & (1 << n)) >> n) : 0;
    let bit = |n: i32| {
        let b = _mm256_set1_epi32(1 << n);
        let mut tmp = _mm256_and_si256(tuple_vec, b);
        tmp = _mm256_srl_epi32(tmp, _mm_cvtsi32_si128(n));
        tmp = _mm256_sub_epi32(uq_vec, tmp);
        let tmp1 = _mm256_and_si256(rho_vec, b);
        let mask = avx2_cmpneq_epi32(tmp1, zero);
        _mm256_and_si256(mask, tmp)
    };
    m_vec[0] = bit(0);
    m_vec[1] = bit(1);
    m_vec[2] = bit(2);
    m_vec[3] = bit(3);

    rotate_matrix(&mut m_vec);
    rotate_matrix(s_vec);

    // Each iteration process 8 bytes * 2 lines
    for i in 0..4 {
        // cwd = s[i * 4 + 0] & ((1U << m) - 1); cwd_len = m
        let cwd_len = lanes(m_vec[i]);
        let mut tmp = _mm256_sllv_epi32(one, m_vec[i]);
        tmp = _mm256_sub_epi32(tmp, one);
        tmp = _mm256_and_si256(tmp, s_vec[i]);
        let cwd = lanes(tmp);

        let mut j = 0;
        while j < 4 {
            let idx0 = j * 2;
            let mut c = u64::from(cwd[idx0]);
            let mut l = cwd_len[idx0] as i32;
            c |= u64::from(cwd[idx0 + 1]).wrapping_shl(l as u32);
            l += cwd_len[idx0 + 1] as i32;

            let idx1 = (j + 1) * 2;
            let len1 = cwd_len[idx1] as i32 + cwd_len[idx1 + 1] as i32;
            if l + len1 <= 64 {
                c |= u64::from(cwd[idx1]).wrapping_shl(l as u32);
                l += cwd_len[idx1] as i32;
                c |= u64::from(cwd[idx1 + 1]).wrapping_shl(l as u32);
                l += cwd_len[idx1 + 1] as i32;
                msp.encode_nodefer(c, l)?;
            } else {
                msp.encode_nodefer(c, l)?;
                c = u64::from(cwd[idx1]);
                l = cwd_len[idx1] as i32;
                c |= u64::from(cwd[idx1 + 1]).wrapping_shl(l as u32);
                l += cwd_len[idx1 + 1] as i32;
                msp.encode_nodefer(c, l)?;
            }
            j += 2;
        }
    }
    msp.drain()
}

/// Port of `cal_eps_vec`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:884-915 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn cal_eps_vec(eq_vec: &[__m256i; 4], u_q_vec: __m256i, e_qmax_vec: __m256i) -> __m256i {
    let u_q_mask = _mm256_cmpgt_epi32(u_q_vec, _mm256_setzero_si256());

    let mut mask = _mm256_cmpeq_epi32(eq_vec[0], e_qmax_vec);
    let mut eps_vec = _mm256_srli_epi32::<31>(mask);

    mask = _mm256_cmpeq_epi32(eq_vec[1], e_qmax_vec);
    let mut tmp = _mm256_srli_epi32::<31>(mask);
    tmp = _mm256_slli_epi32::<1>(tmp);
    eps_vec = _mm256_or_si256(eps_vec, tmp);

    mask = _mm256_cmpeq_epi32(eq_vec[2], e_qmax_vec);
    tmp = _mm256_srli_epi32::<31>(mask);
    tmp = _mm256_slli_epi32::<2>(tmp);
    eps_vec = _mm256_or_si256(eps_vec, tmp);

    mask = _mm256_cmpeq_epi32(eq_vec[3], e_qmax_vec);
    tmp = _mm256_srli_epi32::<31>(mask);
    tmp = _mm256_slli_epi32::<3>(tmp);
    eps_vec = _mm256_or_si256(eps_vec, tmp);

    _mm256_and_si256(u_q_mask, eps_vec)
}

/// Port of `update_lep` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:917-929
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn update_lep(
    x: usize,
    prev_e_val_vec: &mut __m256i,
    eq_vec: &[__m256i; 4],
    e_val_vec: &mut [__m256i; 65],
    left_shift: __m256i,
) {
    let mut tmp = _mm256_permutevar8x32_epi32(eq_vec[3], left_shift);
    tmp = _mm256_insert_epi32::<0>(
        tmp,
        _mm_cvtsi128_si32(_mm256_castsi256_si128(*prev_e_val_vec)),
    );
    *prev_e_val_vec =
        _mm256_insert_epi32::<0>(_mm256_setzero_si256(), _mm256_extract_epi32::<7>(eq_vec[3]));
    e_val_vec[x] = _mm256_max_epi32(eq_vec[1], tmp);
}

/// Port of `update_lcxp` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:932-950
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn update_lcxp(
    x: usize,
    prev_cx_val_vec: &mut __m256i,
    rho_vec: __m256i,
    cx_val_vec: &mut [__m256i; 65],
    left_shift: __m256i,
) {
    let mut tmp = _mm256_permutevar8x32_epi32(rho_vec, left_shift);
    tmp = _mm256_insert_epi32::<0>(
        tmp,
        _mm_cvtsi128_si32(_mm256_castsi256_si128(*prev_cx_val_vec)),
    );
    *prev_cx_val_vec =
        _mm256_insert_epi32::<0>(_mm256_setzero_si256(), _mm256_extract_epi32::<7>(rho_vec));

    tmp = _mm256_and_si256(tmp, _mm256_set1_epi32(8));
    tmp = _mm256_srli_epi32::<3>(tmp);

    let mut tmp1 = _mm256_and_si256(rho_vec, _mm256_set1_epi32(2));
    tmp1 = _mm256_srli_epi32::<1>(tmp1);
    cx_val_vec[x] = _mm256_or_si256(tmp, tmp1);
}

/// Port of `cal_tuple` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:952-961
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn cal_tuple(
    cq_vec: __m256i,
    rho_vec: __m256i,
    eps_vec: __m256i,
    vlc_tbl: &[u32; 2048],
) -> __m256i {
    // tuple[i] = vlc_tbl1[(c_q[i] << 8) + (rho[i] << 4) + eps[i]];
    let mut tmp = _mm256_slli_epi32::<8>(cq_vec);
    let tmp1 = _mm256_slli_epi32::<4>(rho_vec);
    tmp = _mm256_add_epi32(tmp, tmp1);
    tmp = _mm256_add_epi32(tmp, eps_vec);
    // the indices: c_q < 8, rho < 16, eps < 16, so below 2048 (the mask changes none)
    let idx = _mm256_and_si256(tmp, _mm256_set1_epi32(2047));
    // SAFETY: every index is below the table's 2048 entries
    unsafe { _mm256_i32gather_epi32::<4>(vlc_tbl.as_ptr() as *const i32, idx) }
}

/// Port of `proc_cq1` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:963-974
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_cq1(rho_vec: __m256i) -> __m256i {
    // c_q[i + 1] = (rho[i] >> 1) | (rho[i] & 1);
    let tmp = _mm256_srli_epi32::<1>(rho_vec);
    let tmp1 = _mm256_and_si256(rho_vec, _mm256_set1_epi32(1));
    _mm256_or_si256(tmp, tmp1)
}

/// Port of `proc_cq2` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:976-1008
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_cq2(
    x: usize,
    cx_val_vec: &[__m256i; 65],
    rho_vec: __m256i,
    right_shift: __m256i,
) -> __m256i {
    // c_q[i + 1] = (lcxp[i + 1] + (lcxp[i + 2] << 2))
    //            | (((rho[i] & 4) >> 1) | ((rho[i] & 8) >> 2));
    let lcxp1_vec = _mm256_permutevar8x32_epi32(cx_val_vec[x], right_shift);
    let mut tmp = _mm256_permutevar8x32_epi32(lcxp1_vec, right_shift);
    tmp = _mm256_insert_epi64::<3>(
        tmp,
        _mm_cvtsi128_si64(_mm256_castsi256_si128(cx_val_vec[x + 1])),
    );
    tmp = _mm256_slli_epi32::<2>(tmp);
    let mut tmp1 = _mm256_insert_epi32::<7>(
        lcxp1_vec,
        _mm_cvtsi128_si32(_mm256_castsi256_si128(cx_val_vec[x + 1])),
    );
    tmp = _mm256_add_epi32(tmp1, tmp);

    tmp1 = _mm256_and_si256(rho_vec, _mm256_set1_epi32(4));
    tmp1 = _mm256_srli_epi32::<1>(tmp1);
    tmp = _mm256_or_si256(tmp, tmp1);

    tmp1 = _mm256_and_si256(rho_vec, _mm256_set1_epi32(8));
    tmp1 = _mm256_srli_epi32::<2>(tmp1);

    _mm256_or_si256(tmp, tmp1)
}

/// Port of `proc_mel_encode1`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1010-1051 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_mel_encode1(
    melp: &mut Mel,
    cq_vec: __m256i,
    rho_vec: __m256i,
    u_q_vec: __m256i,
    ignore: u32,
    right_shift: __m256i,
) -> OjphResult<()> {
    let zero = _mm256_setzero_si256();
    // if (c_q[i] == 0) mel_encode(&mel, rho[i] != 0);
    let mel_need_encode = lanes(_mm256_cmpeq_epi32(cq_vec, zero));
    let mel_bit = lanes(_mm256_srli_epi32::<31>(avx2_cmpneq_epi32(rho_vec, zero)));

    // mel_encode(&mel, ojph_min(u_q[i], u_q[i + 1]) > 2);
    let tmp = _mm256_permutevar8x32_epi32(u_q_vec, right_shift);
    let tmp1 = _mm256_min_epi32(u_q_vec, tmp);
    let mel_bit2 = lanes(_mm256_srli_epi32::<31>(_mm256_cmpgt_epi32(
        tmp1,
        _mm256_set1_epi32(2),
    )));

    // if (u_q[i] > 0 && u_q[i + 1] > 0) { }
    let need_encode2 = _mm256_cmpgt_epi32(u_q_vec, zero);
    let mel_need_encode2 = lanes(_mm256_and_si256(
        need_encode2,
        _mm256_cmpgt_epi32(tmp, zero),
    ));

    let i_max = (8 - (ignore / 2)) as usize;

    let mut i = 0;
    while i < i_max {
        if mel_need_encode[i] != 0 {
            melp.encode(mel_bit[i] != 0)?;
        }

        if i + 1 < i_max && mel_need_encode[i + 1] != 0 {
            melp.encode(mel_bit[i + 1] != 0)?;
        }

        if mel_need_encode2[i] != 0 {
            melp.encode(mel_bit2[i] != 0)?;
        }
        i += 2;
    }
    Ok(())
}

/// Port of `proc_mel_encode2`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1053-1081 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx2")]
fn proc_mel_encode2(
    melp: &mut Mel,
    cq_vec: __m256i,
    rho_vec: __m256i,
    ignore: u32,
) -> OjphResult<()> {
    let zero = _mm256_setzero_si256();
    let need = _mm256_cmpeq_epi32(cq_vec, zero);
    let mut mask = _mm256_movemask_epi8(need) as u32;
    mask &= 0x88888888;

    let i_max = 8 - (ignore / 2);
    if i_max < 8 {
        mask &= (1u32 << (i_max * 4)) - 1;
    }

    if mask == 0 {
        return Ok(());
    }

    let mel_bit = lanes(_mm256_srli_epi32::<31>(avx2_cmpneq_epi32(rho_vec, zero)));

    while mask != 0 {
        let bit_pos = mask.trailing_zeros();
        let i = (bit_pos / 4) as usize;
        melp.encode(mel_bit[i] != 0)?;
        mask &= mask - 1;
    }
    Ok(())
}

/// Port of `build_vlc_uvlc_pair`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1086-1099 @ OpenJPH 0.32.0).
#[inline]
fn build_vlc_uvlc_pair(
    tuple: &[u32; 10],
    u_q: &[u32; 10],
    i: usize,
    uvlc_tbl: &[u32; 33 * 33],
) -> (u64, i32) {
    let mut val = u64::from(tuple[i] >> 4);
    let mut size = (tuple[i] & 7) as i32;

    val |= u64::from(tuple[i + 1] >> 4) << size;
    size += (tuple[i + 1] & 7) as i32;

    let entry = uvlc_tbl[u_q[i] as usize * 33 + u_q[i + 1] as usize];
    val |= u64::from(entry >> 5) << size;
    size += (entry & 0x1F) as i32;
    (val, size)
}

/// Port of `proc_vlc_encode`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1101-1119 @ OpenJPH 0.32.0).
fn proc_vlc_encode(
    vlcp: &mut Vlc,
    tuple: &[u32; 10],
    u_q: &[u32; 10],
    ignore: u32,
    uvlc_tbl: &[u32; 33 * 33],
) -> OjphResult<()> {
    let i_max = (8 - (ignore / 2)) as usize;

    let mut i = 0;
    while i + 2 < i_max {
        let (val1, size1) = build_vlc_uvlc_pair(tuple, u_q, i, uvlc_tbl);
        let (val2, size2) = build_vlc_uvlc_pair(tuple, u_q, i + 2, uvlc_tbl);
        vlcp.encode(val1 | (val2 << size1), size1 + size2)?;
        i += 4;
    }
    if i < i_max {
        let (val, size) = build_vlc_uvlc_pair(tuple, u_q, i, uvlc_tbl);
        vlcp.encode(val, size)?;
    }
    Ok(())
}

/// The encoder's state across a codeblock's lines.
struct State {
    mel: Mel,
    vlc: Vlc,
    ms: Ms,
    e_val_vec: [__m256i; 65],
    prev_e_val_vec: __m256i,
    cx_val_vec: [__m256i; 65],
    prev_cx_val_vec: __m256i,
    prev_cq: u32,
}

/// Port of `encode_x_loop`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1121-1235 @ OpenJPH 0.32.0):
/// the two lines at `sp` of `buf`, `first` for the codeblock's first pair (`PASS == 1`).
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
fn encode_x_loop(
    first: bool,
    buf: &[u32],
    mut sp: usize,
    stride: usize,
    height: u32,
    y: u32,
    n_loop: u32,
    width_in: u32,
    ignore: u32,
    p: u32,
    st: &mut State,
    right_shift: __m256i,
    left_shift: __m256i,
) -> OjphResult<()> {
    let tb = tables();
    let vlc_tbl = if first { &tb.vlc_tbl0 } else { &tb.vlc_tbl1 };
    let zero = _mm256_setzero_si256();
    let one = _mm256_set1_epi32(1);
    let mut eq_vec = [zero; 4];
    let mut s_vec = [zero; 4];
    let mut src_vec = [zero; 4];

    // 16 samples per iteration
    for x in 0..n_loop as usize {
        // t = sp[i];
        let load = |at: usize, n: usize| -> [__m256i; 2] {
            let mut tmp_buf = [0u32; 16];
            tmp_buf[..n].copy_from_slice(&buf[at..at + n]);
            // SAFETY: tmp_buf holds 16 u32
            unsafe {
                [
                    _mm256_loadu_si256(tmp_buf.as_ptr() as *const __m256i),
                    _mm256_loadu_si256(tmp_buf.as_ptr().add(8) as *const __m256i),
                ]
            }
        };
        let n = if x == (n_loop - 1) as usize && (width_in % 16) != 0 {
            (width_in % 16) as usize
        } else {
            16
        };
        // (upstream loads 16 samples a line where they are all there, else copies the row's
        // last ones into a zeroed buffer; the lanes are the same)
        let [a, b] = load(sp, n);
        src_vec[0] = a;
        src_vec[2] = b;
        if y + 1 < height {
            let [a, b] = load(sp + stride, n);
            src_vec[1] = a;
            src_vec[3] = b;
        } else {
            src_vec[1] = zero;
            src_vec[3] = zero;
        }
        if n == 16 {
            sp += 16;
        }

        let mut rho_vec = zero;
        let mut e_qmax_vec = zero;
        proc_pixel(
            &src_vec,
            p,
            &mut eq_vec,
            &mut s_vec,
            &mut rho_vec,
            &mut e_qmax_vec,
        );

        // max_e[(i + 1) % num] = ojph_max(lep[i + 1], lep[i + 2]) - 1;
        let mut tmp = _mm256_permutevar8x32_epi32(st.e_val_vec[x], right_shift);
        tmp = _mm256_insert_epi32::<7>(
            tmp,
            _mm_cvtsi128_si32(_mm256_castsi256_si128(st.e_val_vec[x + 1])),
        );

        let mut max_e_vec = _mm256_max_epi32(tmp, st.e_val_vec[x]);
        max_e_vec = _mm256_sub_epi32(max_e_vec, one);

        // kappa[i] = (rho[i] & (rho[i] - 1)) ? ojph_max(1, max_e[i]) : 1;
        tmp = _mm256_max_epi32(max_e_vec, one);
        let mut tmp1 = _mm256_sub_epi32(rho_vec, one);
        tmp1 = _mm256_and_si256(rho_vec, tmp1);

        let cmp = _mm256_cmpeq_epi32(tmp1, zero);
        let kappa_vec1 = _mm256_and_si256(cmp, one);
        let kappa_vec2 = _mm256_and_si256(_mm256_xor_si256(cmp, _mm256_set1_epi32(-1)), tmp);
        let kappa_vec = _mm256_max_epi32(kappa_vec1, kappa_vec2);

        tmp = if first {
            proc_cq1(rho_vec)
        } else {
            proc_cq2(x, &st.cx_val_vec, rho_vec, right_shift)
        };

        let mut cq_vec = _mm256_permutevar8x32_epi32(tmp, left_shift);
        cq_vec = _mm256_insert_epi32::<0>(cq_vec, st.prev_cq as i32);
        st.prev_cq = _mm256_extract_epi32::<7>(tmp) as u32;

        update_lep(
            x,
            &mut st.prev_e_val_vec,
            &eq_vec,
            &mut st.e_val_vec,
            left_shift,
        );
        update_lcxp(
            x,
            &mut st.prev_cx_val_vec,
            rho_vec,
            &mut st.cx_val_vec,
            left_shift,
        );

        // Uq[i] = ojph_max(e_qmax[i], kappa[i]); u_q[i] = Uq[i] - kappa[i];
        let uq_vec = _mm256_max_epi32(kappa_vec, e_qmax_vec);
        let u_q_vec = _mm256_sub_epi32(uq_vec, kappa_vec);

        let eps_vec = cal_eps_vec(&eq_vec, u_q_vec, e_qmax_vec);
        let mut tuple_vec = cal_tuple(cq_vec, rho_vec, eps_vec, vlc_tbl);
        let ignore_here = if (n_loop - 1) as usize == x {
            ignore
        } else {
            0
        };

        if first {
            proc_mel_encode1(
                &mut st.mel,
                cq_vec,
                rho_vec,
                u_q_vec,
                ignore_here,
                right_shift,
            )?;
        } else {
            proc_mel_encode2(&mut st.mel, cq_vec, rho_vec, ignore_here)?;
        }

        proc_ms_encode(&mut st.ms, tuple_vec, uq_vec, rho_vec, &mut s_vec)?;

        tuple_vec = _mm256_srli_epi32::<4>(tuple_vec);
        let mut tuple = [0u32; 10];
        let mut u_q = [0u32; 10];
        tuple[..8].copy_from_slice(&lanes(tuple_vec));
        u_q[..8].copy_from_slice(&lanes(u_q_vec));
        {
            let i_max = (8 - (ignore_here / 2)) as usize;
            if (i_max & 1) != 0 {
                tuple[i_max] = 0;
                u_q[i_max] = 0;
            }
            tuple[8] = 0;
            u_q[8] = 0;
        }
        let uvlc = if first {
            &tb.uvlc_tbl_pair1
        } else {
            &tb.uvlc_tbl_pair2
        };
        proc_vlc_encode(&mut st.vlc, &tuple, &u_q, ignore_here, uvlc)?;
    }
    Ok(())
}

/// Port of `ojph_encode_codeblock_avx2`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx2.cpp:1237-1349 @ OpenJPH 0.32.0):
/// the codeblock's cleanup pass; the coded bytes in `coded`, as the generic encoder gives
/// them (between its prefix and suffix padding).
///
/// # Safety
/// The CPU supports AVX2; `buf` holds `height` rows of `stride` samples, the last at least
/// `width` long, `width <= 1024`.
#[target_feature(enable = "avx2")]
#[allow(clippy::too_many_arguments)]
unsafe fn ojph_encode_codeblock_avx2(
    buf: &[u32],
    missing_msbs: u32,
    _num_passes: u32,
    width_in: u32,
    height: u32,
    stride: u32,
    lengths: &mut [u32; 2],
    coded: &mut Option<crate::mem::Pooled<u8>>,
) -> OjphResult<()> {
    let width = (width_in + 15) & !15;
    let ignore = width - width_in;
    let p = 30 - missing_msbs;
    let zero = _mm256_setzero_si256();

    let right_shift = _mm256_set_epi32(0, 7, 6, 5, 4, 3, 2, 1);
    let left_shift = _mm256_set_epi32(6, 5, 4, 3, 2, 1, 0, 7);

    let n_loop = (width + 15) / 16;

    // (upstream zeroes e_val_vec's first min(64, n_loop) entries and leaves cx_val_vec
    // uninitialized; every entry read is zeroed or written first)
    let mut st = State {
        mel: Mel::new(),
        vlc: Vlc::new(),
        ms: Ms::new(),
        e_val_vec: [zero; 65],
        prev_e_val_vec: zero,
        cx_val_vec: [zero; 65],
        prev_cx_val_vec: zero,
        prev_cq: 0,
    };

    let stride = stride as usize;
    // 2 lines per iteration
    let mut y = 0;
    while y < height {
        st.e_val_vec[n_loop as usize] = st.prev_e_val_vec;
        // lcxp[0] = (ui8)((rho[0] & 8) >> 3);
        let tmp = _mm256_and_si256(st.prev_cx_val_vec, _mm256_set1_epi32(8));
        st.cx_val_vec[n_loop as usize] = _mm256_srli_epi32::<3>(tmp);

        st.prev_e_val_vec = zero;
        st.prev_cx_val_vec = zero;

        let sp = y as usize * stride;
        encode_x_loop(
            y == 0,
            buf,
            sp,
            stride,
            height,
            y,
            n_loop,
            width_in,
            ignore,
            p,
            &mut st,
            right_shift,
            left_shift,
        )?;

        let mut tmp = _mm256_permutevar8x32_epi32(st.cx_val_vec[0], right_shift);
        tmp = _mm256_slli_epi32::<2>(tmp);
        tmp = _mm256_add_epi32(tmp, st.cx_val_vec[0]);
        st.prev_cq = _mm_cvtsi128_si32(_mm256_castsi256_si128(tmp)) as u32;
        y += 2;
    }

    st.ms.terminate()?;
    st.vlc.drain()?;
    terminate_mel_vlc(&mut st.mel, &mut st.vlc)?;

    // copy to elastic
    let (mel, vlc, ms) = (&st.mel, &st.vlc, &st.ms);
    let (mp, vp, sp) = (mel.pos as usize, vlc.pos as usize, ms.pos as usize);
    lengths[0] = (mp + vp + sp) as u32;
    let n = lengths[0] as usize;
    let mut out = crate::mem::Pooled::zeroed(PREFIX_BUF_SIZE + n + SUFFIX_BUF_SIZE);
    let c = &mut out[PREFIX_BUF_SIZE..PREFIX_BUF_SIZE + n];
    c[..sp].copy_from_slice(&ms.buf[..sp]);
    c[sp..sp + mp].copy_from_slice(&mel.buf[..mp]);
    c[sp + mp..].copy_from_slice(&vlc.buf[VLC_SIZE - vp..]);

    // put in the interface locator word
    let num_bytes = (mp + vp) as u32;
    c[n - 1] = (num_bytes >> 4) as u8;
    c[n - 2] &= 0xF0;
    c[n - 2] |= (num_bytes & 0xF) as u8;
    *coded = Some(out);
    Ok(())
}

/// [`ojph_encode_codeblock_avx2`] where the CPU has AVX2 (upstream's level check implies it)
/// and the buffer holds the codeblock; `None` otherwise.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_codeblock_avx2(
    buf: &[u32],
    missing_msbs: u32,
    num_passes: u32,
    width: u32,
    height: u32,
    stride: u32,
    lengths: &mut [u32; 2],
    coded: &mut Option<crate::mem::Pooled<u8>>,
) -> Option<OjphResult<()>> {
    if !std::arch::is_x86_feature_detected!("avx2") {
        return None;
    }
    if width == 0
        || height == 0
        || width > 1024
        || stride < width
        || ((height - 1) as usize) * (stride as usize) + (width as usize) > buf.len()
    {
        return None;
    }
    // SAFETY: the CPU has AVX2, and the buffer holds the codeblock
    Some(unsafe {
        ojph_encode_codeblock_avx2(
            buf,
            missing_msbs,
            num_passes,
            width,
            height,
            stride,
            lengths,
            coded,
        )
    })
}
