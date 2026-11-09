// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_encoder_avx512.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:1-1218 @ OpenJPH 0.32.0):
//! the HT block encoder upstream chooses at `X86_CPU_EXT_LEVEL_AVX512` (`codeblock_fun::init`;
//! built with `-mavx512f -mavx512cd`, `/arch:AVX512` under MSVC), 32 samples of two lines
//! per step in AVX-512.
//!
//! Upstream's `mel_emit_bit` and `vlc_encode` write without checking their buffers, which
//! are "more than enough" for a codeblock's 4096 samples; here a write past them is refused
//! with the generic encoder's messages, which no codeblock reaches.

#![allow(unsafe_code)]

use std::arch::x86_64::*;
use std::sync::OnceLock;

use crate::arch::population_count;
use crate::bitbuffer_read::{PREFIX_BUF_SIZE, SUFFIX_BUF_SIZE};
use crate::block_common::{TBL0, TBL1};
use crate::message::{OjphResult, error};

const FILE: &str = "ojph_block_encoder_avx512.cpp";
/// The generic encoder's file, whose messages a write past a buffer gives.
const GEN_FILE: &str = "ojph_block_encoder.cpp";

/// The encoder's tables (`vlc_tbl0`, `vlc_tbl1`, and the UVLC codewords `ulvc_cwd_*`).
struct Tables {
    vlc_tbl0: [u32; 2048],
    vlc_tbl1: [u32; 2048],
    ulvc_cwd_pre: [u32; 33],
    ulvc_cwd_pre_len: [i32; 33],
    ulvc_cwd_suf: [u32; 33],
    ulvc_cwd_suf_len: [i32; 33],
}

/// Port of one table of `vlc_init_tables`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:80-197 @ OpenJPH 0.32.0).
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

/// Port of `initialize_block_encoder_tables_avx512`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:80-234 @ OpenJPH 0.32.0),
/// with `vlc_init_tables` and `uvlc_init_tables`.
fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = Tables {
            vlc_tbl0: [0; 2048],
            vlc_tbl1: [0; 2048],
            ulvc_cwd_pre: [0; 33],
            ulvc_cwd_pre_len: [0; 33],
            ulvc_cwd_suf: [0; 33],
            ulvc_cwd_suf_len: [0; 33],
        };
        vlc_init_table(&mut t.vlc_tbl0, &TBL0);
        vlc_init_table(&mut t.vlc_tbl1, &TBL1);
        // code goes from 0 to 31, extension and 32 are not supported here
        t.ulvc_cwd_pre[..5].copy_from_slice(&[0, 1, 2, 4, 4]);
        t.ulvc_cwd_pre_len[..5].copy_from_slice(&[0, 1, 2, 3, 3]);
        t.ulvc_cwd_suf[..5].copy_from_slice(&[0, 0, 0, 0, 1]);
        t.ulvc_cwd_suf_len[..5].copy_from_slice(&[0, 0, 0, 1, 1]);
        for i in 5..33 {
            t.ulvc_cwd_pre[i] = 0;
            t.ulvc_cwd_pre_len[i] = 3;
            t.ulvc_cwd_suf[i] = (i - 5) as u32;
            t.ulvc_cwd_suf_len[i] = 5;
        }
        t
    })
}

/// `mel_size` and `vlc_size`: the parts of upstream's `mel_vlc_buf` of 3072 bytes.
const MEL_SIZE: usize = 192;
const VLC_SIZE: usize = 3072 - MEL_SIZE;
/// `ms_size`.
const MS_SIZE: usize = (16384 * 16 + 14) / 15;

/// Port of `mel_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:239-251
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
    /// Port of `mel_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:254-265
    /// @ OpenJPH 0.32.0).
    fn new() -> Self {
        Mel {
            buf: [0; MEL_SIZE],
            pos: 0,
            remaining_bits: 8,
            tmp: 0,
            run: 0,
            k: 0,
            threshold: 1, // this is 1 << mel_exp[melp->k]
        }
    }

    /// Port of `mel_emit_bit`
    /// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:268-278 @ OpenJPH
    /// 0.32.0), its buffer checked (see the module).
    #[inline]
    fn emit_bit(&mut self, v: i32) -> OjphResult<()> {
        self.tmp = (self.tmp << 1) + v;
        self.remaining_bits -= 1;
        if self.remaining_bits == 0 {
            let Some(b) = self.buf.get_mut(self.pos as usize) else {
                return Err(error(
                    0x00020001,
                    GEN_FILE,
                    311,
                    "mel encoder's buffer is full",
                    &[],
                ));
            };
            *b = self.tmp as u8;
            self.pos += 1;
            self.remaining_bits = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
        }
        Ok(())
    }

    /// Port of `mel_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:281-305
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn encode(&mut self, bit: bool) -> OjphResult<()> {
        // MEL exponent
        const MEL_EXP: [i32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

        if !bit {
            self.run += 1;
            if self.run >= self.threshold {
                self.emit_bit(1)?;
                self.run = 0;
                self.k = if 12 < self.k + 1 { 12 } else { self.k + 1 };
                self.threshold = 1 << MEL_EXP[self.k as usize];
            }
        } else {
            self.emit_bit(0)?;
            let mut t = MEL_EXP[self.k as usize];
            while t > 0 {
                t -= 1;
                self.emit_bit((self.run >> t) & 1)?;
            }
            self.run = 0;
            self.k = if 0 > self.k - 1 { 0 } else { self.k - 1 };
            self.threshold = 1 << MEL_EXP[self.k as usize];
        }
        Ok(())
    }
}

/// Port of `vlc_struct_avx512`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:310-319 @ OpenJPH 0.32.0):
/// written backward from the end of `buf`.
struct Vlc {
    buf: [u8; VLC_SIZE],
    pos: u32,
    used_bits: i32,
    tmp: u64,
    last_greater_than_8f: bool,
}

impl Vlc {
    /// Port of `vlc_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:322-333
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

    /// Port of `vlc_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:336-371
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn encode(&mut self, cwd: u32, cwd_len: i32) -> OjphResult<()> {
        self.tmp |= u64::from(cwd) << self.used_bits;
        self.used_bits += cwd_len;

        while self.used_bits >= 8 {
            if self.last_greater_than_8f {
                let tmp = (self.tmp & 0x7F) as u8;
                if tmp != 0x7F {
                    let tmp = (self.tmp & 0xFF) as u8;
                    self.put(tmp)?;
                    self.last_greater_than_8f = tmp > 0x8F;
                    self.tmp >>= 8;
                    self.used_bits -= 8;
                } else {
                    self.put(tmp)?;
                    self.last_greater_than_8f = false;
                    self.tmp >>= 7;
                    self.used_bits -= 7;
                }
            } else {
                let tmp = (self.tmp & 0xFF) as u8;
                self.put(tmp)?;
                self.last_greater_than_8f = tmp > 0x8F;
                self.tmp >>= 8;
                self.used_bits -= 8;
            }
            self.pos += 1;
        }
        Ok(())
    }
}

/// Port of `terminate_mel_vlc`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:376-413 @ OpenJPH 0.32.0).
fn terminate_mel_vlc(mel: &mut Mel, vlc: &mut Vlc) -> OjphResult<()> {
    if mel.run > 0 {
        mel.emit_bit(1)?;
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
            396,
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
                408,
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

/// Port of `ms_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:418-427
/// @ OpenJPH 0.32.0).
struct Ms {
    buf: crate::mem::Pooled<u8>,
    pos: u32,
    max_bits: i32,
    used_bits: i32,
    tmp: u32,
}

impl Ms {
    /// Port of `ms_init` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:430-439
    /// @ OpenJPH 0.32.0).
    fn new() -> Self {
        Ms {
            buf: crate::mem::Pooled::zeroed(MS_SIZE),
            pos: 0,
            max_bits: 8,
            used_bits: 0,
            tmp: 0,
        }
    }

    /// Port of `ms_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:442-462
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn encode(&mut self, mut cwd: u64, mut cwd_len: i32) -> OjphResult<()> {
        while cwd_len > 0 {
            if self.pos as usize >= MS_SIZE {
                return Err(error(
                    0x00020005,
                    FILE,
                    448,
                    "magnitude sign encoder's buffer is full",
                    &[],
                ));
            }
            let room = self.max_bits - self.used_bits;
            let t = if room < cwd_len { room } else { cwd_len };
            self.tmp |= ((cwd & u64::from(1u32.wrapping_shl(t as u32).wrapping_sub(1))) as u32)
                << self.used_bits;
            self.used_bits += t;
            cwd >>= t;
            cwd_len -= t;
            if self.used_bits >= self.max_bits {
                self.buf[self.pos as usize] = self.tmp as u8;
                self.pos += 1;
                self.max_bits = if self.tmp == 0xFF { 7 } else { 8 };
                self.tmp = 0;
                self.used_bits = 0;
            }
        }
        Ok(())
    }

    /// Port of `ms_terminate`
    /// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:465-482 @ OpenJPH 0.32.0).
    fn terminate(&mut self) -> OjphResult<()> {
        if self.used_bits != 0 {
            let t = self.max_bits - self.used_bits; // unused bits
            self.tmp |= (0xFF & ((1u32 << t) - 1)) << self.used_bits;
            self.used_bits += t;
            if self.tmp != 0xFF {
                if self.pos as usize >= MS_SIZE {
                    return Err(error(
                        0x00020006,
                        FILE,
                        476,
                        "magnitude sign encoder's buffer is full",
                        &[],
                    ));
                }
                self.buf[self.pos as usize] = self.tmp as u8;
                self.pos += 1;
            }
        } else if self.max_bits == 7 {
            self.pos -= 1;
        }
        Ok(())
    }
}

/// Port of `proc_pixel` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:502-595
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f,avx512cd")]
fn proc_pixel(
    src_vec: &[__m512i; 4],
    p: u32,
    eq_vec: &mut [__m512i; 4],
    s_vec: &mut [__m512i; 4],
    rho_vec: &mut __m512i,
    e_qmax_vec: &mut __m512i,
) {
    let zero = _mm512_setzero_si512();
    let one = _mm512_set1_epi32(1);
    let mut val_vec = [zero; 4];
    let mut eq = [zero; 4];
    let mut s = [zero; 4];
    let mut rho = [zero; 4];
    let mut val_mask = [0u16; 4];
    let count = _mm_cvtsi32_si128(p as i32);

    for i in 0..4 {
        // val = t + t; //multiply by 2 and get rid of sign
        val_vec[i] = _mm512_add_epi32(src_vec[i], src_vec[i]);
        // val >>= p;  // 2 \mu_p + x
        val_vec[i] = _mm512_srl_epi32(val_vec[i], count);
        // val &= ~1u; // 2 \mu_p
        val_vec[i] = _mm512_and_epi32(val_vec[i], _mm512_set1_epi32(!1u32 as i32));
        // if (val) {
        val_mask[i] = _mm512_cmpneq_epi32_mask(val_vec[i], zero);
        // e_q[i] = 32 - (int)count_leading_ZEROs(--val); //2\mu_p - 1
        val_vec[i] = _mm512_mask_sub_epi32(zero, val_mask[i], val_vec[i], one);
        eq[i] = _mm512_mask_lzcnt_epi32(zero, val_mask[i], val_vec[i]);
        eq[i] = _mm512_mask_sub_epi32(zero, val_mask[i], _mm512_set1_epi32(32), eq[i]);
        // s[0] = --val + (t >> 31); //v_n = 2(\mu_p-1) + s_n
        val_vec[i] = _mm512_mask_sub_epi32(zero, val_mask[i], val_vec[i], one);
        s[i] = _mm512_mask_srli_epi32::<31>(zero, val_mask[i], src_vec[i]);
        s[i] = _mm512_mask_add_epi32(zero, val_mask[i], s[i], val_vec[i]);
    }

    for i in 0..4 {
        val_vec[i] = _mm512_mask_mov_epi32(zero, val_mask[i], one);
    }
    *e_qmax_vec = zero;

    let idx = [
        _mm512_set_epi32(14, 12, 10, 8, 6, 4, 2, 0, 14, 12, 10, 8, 6, 4, 2, 0),
        _mm512_set_epi32(15, 13, 11, 9, 7, 5, 3, 1, 15, 13, 11, 9, 7, 5, 3, 1),
    ];

    // reorder to [0, 0], [0, 2] ... [0,30] | [1, 0] ... | [0, 1] ... | [1, 1] ...
    for i in 0..4 {
        let e_idx = i >> 1;
        let o_idx = i & 0x1;

        eq_vec[i] = _mm512_permutexvar_epi32(idx[e_idx], eq[o_idx]);
        eq_vec[i] = _mm512_mask_permutexvar_epi32(eq_vec[i], 0xFF00, idx[e_idx], eq[o_idx + 2]);

        s_vec[i] = _mm512_permutexvar_epi32(idx[e_idx], s[o_idx]);
        s_vec[i] = _mm512_mask_permutexvar_epi32(s_vec[i], 0xFF00, idx[e_idx], s[o_idx + 2]);

        rho[i] = _mm512_permutexvar_epi32(idx[e_idx], val_vec[o_idx]);
        rho[i] = _mm512_mask_permutexvar_epi32(rho[i], 0xFF00, idx[e_idx], val_vec[o_idx + 2]);
        rho[i] = _mm512_sll_epi32(rho[i], _mm_cvtsi32_si128(i as i32));

        *e_qmax_vec = _mm512_max_epi32(*e_qmax_vec, eq_vec[i]);
    }

    *rho_vec = _mm512_or_epi32(rho[0], rho[1]);
    *rho_vec = _mm512_or_epi32(*rho_vec, rho[2]);
    *rho_vec = _mm512_or_epi32(*rho_vec, rho[3]);
}

/// Port of `rotate_matrix`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:610-632 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn rotate_matrix(matrix: &mut [__m512i; 4]) {
    let mut m = [
        _mm512_unpacklo_epi32(matrix[0], matrix[1]),
        _mm512_unpackhi_epi32(matrix[0], matrix[1]),
        _mm512_unpacklo_epi32(matrix[2], matrix[3]),
        _mm512_unpackhi_epi32(matrix[2], matrix[3]),
    ];

    matrix[0] = _mm512_unpacklo_epi64(m[0], m[2]);
    matrix[1] = _mm512_unpackhi_epi64(m[0], m[2]);
    matrix[2] = _mm512_unpacklo_epi64(m[1], m[3]);
    matrix[3] = _mm512_unpackhi_epi64(m[1], m[3]);

    m[0] = _mm512_shuffle_i32x4::<0x88>(matrix[0], matrix[1]);
    m[1] = _mm512_shuffle_i32x4::<0x88>(matrix[2], matrix[3]);
    m[2] = _mm512_shuffle_i32x4::<0xDD>(matrix[0], matrix[1]);
    m[3] = _mm512_shuffle_i32x4::<0xDD>(matrix[2], matrix[3]);

    matrix[0] = _mm512_shuffle_i32x4::<0x88>(m[0], m[1]);
    matrix[1] = _mm512_shuffle_i32x4::<0x88>(m[2], m[3]);
    matrix[2] = _mm512_shuffle_i32x4::<0xDD>(m[0], m[1]);
    matrix[3] = _mm512_shuffle_i32x4::<0xDD>(m[2], m[3]);
}

/// The 16 lanes of `v`.
#[target_feature(enable = "avx512f")]
#[inline]
fn lanes(v: __m512i) -> [u32; 16] {
    let mut out = [0u32; 16];
    // SAFETY: out holds 16 u32
    unsafe { _mm512_storeu_si512(out.as_mut_ptr() as *mut _, v) };
    out
}

/// Port of `proc_ms_encode`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:634-713 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn proc_ms_encode(
    msp: &mut Ms,
    tuple_vec: __m512i,
    uq_vec: __m512i,
    rho_vec: __m512i,
    s_vec: &mut [__m512i; 4],
) -> OjphResult<()> {
    let zero = _mm512_setzero_si512();
    let one = _mm512_set1_epi32(1);
    let mut m_vec = [zero; 4];

    // m = (rho[i] & (1 << n)) ? Uq[i] - ((tuple[i] & (1 << n)) >> n) : 0;
    let bit = |n: i32| {
        let b = _mm512_set1_epi32(1 << n);
        let mut tmp = _mm512_and_epi32(tuple_vec, b);
        tmp = _mm512_srl_epi32(tmp, _mm_cvtsi32_si128(n));
        tmp = _mm512_sub_epi32(uq_vec, tmp);
        let tmp1 = _mm512_and_epi32(rho_vec, b);
        let mask = _mm512_cmpneq_epi32_mask(tmp1, zero);
        _mm512_mask_mov_epi32(zero, mask, tmp)
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
        let mut tmp = _mm512_sllv_epi32(one, m_vec[i]);
        tmp = _mm512_sub_epi32(tmp, one);
        tmp = _mm512_and_epi32(tmp, s_vec[i]);
        let cwd = lanes(tmp);

        for j in 0..8 {
            let idx = j * 2;
            let mut c = u64::from(cwd[idx]);
            let mut l = cwd_len[idx] as i32;
            c |= u64::from(cwd[idx + 1]) << l;
            l += cwd_len[idx + 1] as i32;
            msp.encode(c, l)?;
        }
    }
    Ok(())
}

/// Port of `cal_eps_vec`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:715-746 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn cal_eps_vec(eq_vec: &[__m512i; 4], u_q_vec: __m512i, e_qmax_vec: __m512i) -> __m512i {
    let zero = _mm512_setzero_si512();
    let one = _mm512_set1_epi32(1);
    let u_q_mask = _mm512_cmpgt_epi32_mask(u_q_vec, zero);

    let mut mask = _mm512_cmpeq_epi32_mask(eq_vec[0], e_qmax_vec);
    let mut tmp = _mm512_mask_mov_epi32(zero, mask, one);
    let mut eps_vec = _mm512_mask_mov_epi32(zero, u_q_mask, tmp);

    mask = _mm512_cmpeq_epi32_mask(eq_vec[1], e_qmax_vec);
    tmp = _mm512_mask_mov_epi32(zero, mask, one);
    tmp = _mm512_slli_epi32::<1>(tmp);
    eps_vec = _mm512_mask_or_epi32(zero, u_q_mask, eps_vec, tmp);

    mask = _mm512_cmpeq_epi32_mask(eq_vec[2], e_qmax_vec);
    tmp = _mm512_mask_mov_epi32(zero, mask, one);
    tmp = _mm512_slli_epi32::<2>(tmp);
    eps_vec = _mm512_mask_or_epi32(zero, u_q_mask, eps_vec, tmp);

    mask = _mm512_cmpeq_epi32_mask(eq_vec[3], e_qmax_vec);
    tmp = _mm512_mask_mov_epi32(zero, mask, one);
    tmp = _mm512_slli_epi32::<3>(tmp);

    _mm512_mask_or_epi32(zero, u_q_mask, eps_vec, tmp)
}

/// Port of `update_lep` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:748-761
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn update_lep(
    x: usize,
    prev_e_val_vec: &mut __m512i,
    eq_vec: &[__m512i; 4],
    e_val_vec: &mut [__m512i; 33],
    left_shift: __m512i,
) {
    let tmp = _mm512_mask_permutexvar_epi32(*prev_e_val_vec, 0xFFFE, left_shift, eq_vec[3]);
    *prev_e_val_vec =
        _mm512_mask_permutexvar_epi32(_mm512_setzero_si512(), 0x1, left_shift, eq_vec[3]);
    e_val_vec[x] = _mm512_max_epi32(eq_vec[1], tmp);
}

/// Port of `update_lcxp` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:764-783
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn update_lcxp(
    x: usize,
    prev_cx_val_vec: &mut __m512i,
    rho_vec: __m512i,
    cx_val_vec: &mut [__m512i; 33],
    left_shift: __m512i,
) {
    let mut tmp = _mm512_mask_permutexvar_epi32(*prev_cx_val_vec, 0xFFFE, left_shift, rho_vec);
    *prev_cx_val_vec =
        _mm512_mask_permutexvar_epi32(_mm512_setzero_si512(), 0x1, left_shift, rho_vec);

    tmp = _mm512_and_epi32(tmp, _mm512_set1_epi32(8));
    tmp = _mm512_srli_epi32::<3>(tmp);

    let mut tmp1 = _mm512_and_epi32(rho_vec, _mm512_set1_epi32(2));
    tmp1 = _mm512_srli_epi32::<1>(tmp1);
    cx_val_vec[x] = _mm512_or_epi32(tmp, tmp1);
}

/// Port of `cal_tuple` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:785-794
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn cal_tuple(
    cq_vec: __m512i,
    rho_vec: __m512i,
    eps_vec: __m512i,
    vlc_tbl: &[u32; 2048],
) -> __m512i {
    // tuple[i] = vlc_tbl1[(c_q[i] << 8) + (rho[i] << 4) + eps[i]];
    let mut tmp = _mm512_slli_epi32::<8>(cq_vec);
    let tmp1 = _mm512_slli_epi32::<4>(rho_vec);
    tmp = _mm512_add_epi32(tmp, tmp1);
    tmp = _mm512_add_epi32(tmp, eps_vec);
    // the indices: c_q < 8, rho < 16, eps < 16, so below 2048
    let idx = _mm512_and_epi32(tmp, _mm512_set1_epi32(2047));
    // SAFETY: every index is below the table's 2048 entries
    unsafe { _mm512_i32gather_epi32::<4>(idx, vlc_tbl.as_ptr() as *const _) }
}

/// Port of `proc_cq1` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:796-807
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn proc_cq1(rho_vec: __m512i) -> __m512i {
    // c_q[i + 1] = (rho[i] >> 1) | (rho[i] & 1);
    let tmp = _mm512_srli_epi32::<1>(rho_vec);
    let tmp1 = _mm512_and_epi32(rho_vec, _mm512_set1_epi32(1));
    _mm512_or_epi32(tmp, tmp1)
}

/// Port of `proc_cq2` (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:809-830
/// @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn proc_cq2(
    x: usize,
    cx_val_vec: &[__m512i; 33],
    rho_vec: __m512i,
    right_shift: __m512i,
) -> __m512i {
    // c_q[i + 1] = (lcxp[i + 1] + (lcxp[i + 2] << 2))
    //            | (((rho[i] & 4) >> 1) | ((rho[i] & 8) >> 2));
    let lcxp1_vec = _mm512_permutexvar_epi32(right_shift, cx_val_vec[x]);
    let lcxp2_vec = _mm512_permutexvar_epi32(right_shift, cx_val_vec[x + 1]);
    let mut tmp = _mm512_permutexvar_epi32(right_shift, lcxp1_vec);
    tmp = _mm512_mask_permutexvar_epi32(tmp, 0xC000, right_shift, lcxp2_vec);
    tmp = _mm512_slli_epi32::<2>(tmp);
    let mut tmp1 = _mm512_mask_mov_epi32(lcxp1_vec, 0x8000, lcxp2_vec);
    tmp = _mm512_add_epi32(tmp1, tmp);

    tmp1 = _mm512_and_epi32(rho_vec, _mm512_set1_epi32(4));
    tmp1 = _mm512_srli_epi32::<1>(tmp1);
    tmp = _mm512_or_epi32(tmp, tmp1);

    tmp1 = _mm512_and_epi32(rho_vec, _mm512_set1_epi32(8));
    tmp1 = _mm512_srli_epi32::<2>(tmp1);

    _mm512_or_epi32(tmp, tmp1)
}

/// Port of `proc_mel_encode1`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:834-874 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn proc_mel_encode1(
    melp: &mut Mel,
    cq_vec: __m512i,
    rho_vec: __m512i,
    u_q_vec: __m512i,
    ignore: u32,
    right_shift: __m512i,
) -> OjphResult<()> {
    let zero = _mm512_setzero_si512();
    // if (c_q[i] == 0) mel_encode(&mel, rho[i] != 0);
    let mel_need_encode = _mm512_cmpeq_epi32_mask(cq_vec, zero);
    let mel_bit = _mm512_cmpneq_epi32_mask(rho_vec, zero);

    // mel_encode(&mel, ojph_min(u_q[i], u_q[i + 1]) > 2);
    let tmp = _mm512_permutexvar_epi32(right_shift, u_q_vec);
    let tmp1 = _mm512_min_epi32(u_q_vec, tmp);
    let mel_bit2 = _mm512_cmpgt_epi32_mask(tmp1, _mm512_set1_epi32(2));

    // if (u_q[i] > 0 && u_q[i + 1] > 0) { }
    let mut mel_need_encode2 = _mm512_cmpgt_epi32_mask(u_q_vec, zero);
    mel_need_encode2 &= _mm512_cmpgt_epi32_mask(tmp, zero);

    let i_max = 16 - (ignore / 2);

    let mut i = 0;
    while i < i_max {
        let mask = 1u16 << i;
        if (mel_need_encode & mask) != 0 {
            melp.encode((mel_bit & mask) != 0)?;
        }

        if i + 1 < i_max {
            let mask = 1u16 << (i + 1);
            if (mel_need_encode & mask) != 0 {
                melp.encode((mel_bit & mask) != 0)?;
            }
        }

        if (mel_need_encode2 & mask) != 0 {
            melp.encode((mel_bit2 & mask) != 0)?;
        }
        i += 2;
    }
    Ok(())
}

/// Port of `proc_mel_encode2`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:876-898 @ OpenJPH 0.32.0).
#[target_feature(enable = "avx512f")]
fn proc_mel_encode2(
    melp: &mut Mel,
    cq_vec: __m512i,
    rho_vec: __m512i,
    ignore: u32,
) -> OjphResult<()> {
    let zero = _mm512_setzero_si512();
    let mel_need_encode = _mm512_cmpeq_epi32_mask(cq_vec, zero);
    let mel_bit = _mm512_cmpneq_epi32_mask(rho_vec, zero);

    let i_max = 16 - (ignore / 2);
    for i in 0..i_max {
        let mask = 1u16 << i;
        if (mel_need_encode & mask) != 0 {
            melp.encode((mel_bit & mask) != 0)?;
        }
    }
    Ok(())
}

/// Port of `proc_vlc_encode1`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:903-969 @ OpenJPH 0.32.0).
fn proc_vlc_encode1(
    vlcp: &mut Vlc,
    tuple: &[u32; 16],
    u_q: &[u32; 16],
    ignore: u32,
) -> OjphResult<()> {
    let t = tables();
    let (pre, pre_len, suf, suf_len) = (
        &t.ulvc_cwd_pre,
        &t.ulvc_cwd_pre_len,
        &t.ulvc_cwd_suf,
        &t.ulvc_cwd_suf_len,
    );
    let i_max = (16 - (ignore / 2)) as usize;

    let mut i = 0;
    while i < i_max {
        // 7 bits
        let mut val = tuple[i] >> 4;
        let mut size = (tuple[i] & 7) as i32;

        if i + 1 < i_max {
            // 7 bits
            val |= (tuple[i + 1] >> 4) << size;
            size += (tuple[i + 1] & 7) as i32;
        }

        let (a, b) = (u_q[i] as usize, u_q[i + 1] as usize);
        if a > 2 && b > 2 {
            val |= pre[a - 2] << size; // 3 bits
            size += pre_len[a - 2];
            val |= pre[b - 2] << size; // 3 bits
            size += pre_len[b - 2];
            val |= suf[a - 2] << size; // 5 bits
            size += suf_len[a - 2];
            val |= suf[b - 2] << size; // 5 bits
            size += suf_len[b - 2];
        } else if a > 2 && b > 0 {
            val |= pre[a] << size; // 3 bits
            size += pre_len[a];
            val |= (u_q[i + 1] - 1) << size; // 1 bit
            size += 1;
            val |= suf[a] << size; // 5 bits
            size += suf_len[a];
        } else {
            val |= pre[a] << size; // 3 bits
            size += pre_len[a];
            val |= pre[b] << size; // 3 bits
            size += pre_len[b];
            val |= suf[a] << size; // 5 bits
            size += suf_len[a];
            val |= suf[b] << size; // 5 bits
            size += suf_len[b];
        }

        vlcp.encode(val, size)?;
        i += 2;
    }
    Ok(())
}

/// Port of `proc_vlc_encode2`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:971-1005 @ OpenJPH 0.32.0).
fn proc_vlc_encode2(
    vlcp: &mut Vlc,
    tuple: &[u32; 16],
    u_q: &[u32; 16],
    ignore: u32,
) -> OjphResult<()> {
    let t = tables();
    let i_max = (16 - (ignore / 2)) as usize;

    let mut i = 0;
    while i < i_max {
        // 7 bits
        let mut val = tuple[i] >> 4;
        let mut size = (tuple[i] & 7) as i32;

        if i + 1 < i_max {
            // 7 bits
            val |= (tuple[i + 1] >> 4) << size;
            size += (tuple[i + 1] & 7) as i32;
        }

        let (a, b) = (u_q[i] as usize, u_q[i + 1] as usize);
        val |= t.ulvc_cwd_pre[a] << size; // 3 bits
        size += t.ulvc_cwd_pre_len[a];
        val |= t.ulvc_cwd_pre[b] << size; // 3 bits
        size += t.ulvc_cwd_pre_len[b];
        val |= t.ulvc_cwd_suf[a] << size; // 5 bits
        size += t.ulvc_cwd_suf_len[a];
        val |= t.ulvc_cwd_suf[b] << size; // 5 bits
        size += t.ulvc_cwd_suf_len[b];

        vlcp.encode(val, size)?;
        i += 2;
    }
    Ok(())
}

/// Port of `ojph_encode_codeblock_avx512`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder_avx512.cpp:1009-1213 @ OpenJPH 0.32.0):
/// the codeblock's cleanup pass; the coded bytes in `coded`, as the generic encoder gives
/// them (between its prefix and suffix padding).
///
/// # Safety
/// The CPU supports AVX-512F and AVX-512CD; `buf` holds `height` rows of `stride` samples,
/// the last at least `width` long, `width <= 1024`.
#[target_feature(enable = "avx512f,avx512cd")]
#[allow(clippy::too_many_arguments)]
unsafe fn ojph_encode_codeblock_avx512(
    buf: &[u32],
    missing_msbs: u32,
    _num_passes: u32,
    width_in: u32,
    height: u32,
    stride: u32,
    lengths: &mut [u32; 2],
    coded: &mut Option<crate::mem::Pooled<u8>>,
) -> OjphResult<()> {
    let tb = tables();
    let width = (width_in + 31) & !31;
    let ignore = width - width_in;

    let mut mel = Mel::new();
    let mut vlc = Vlc::new();
    let mut ms = Ms::new();

    let p = 30 - missing_msbs;
    let zero = _mm512_setzero_si512();
    let one = _mm512_set1_epi32(1);

    let right_shift = _mm512_set_epi32(0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1);
    let left_shift = _mm512_set_epi32(14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 15);

    let mut e_val_vec = [zero; 33];
    let mut prev_e_val_vec = zero;
    // (upstream leaves cx_val_vec uninitialized; every entry read is written first)
    let mut cx_val_vec = [zero; 33];
    let mut prev_cx_val_vec = zero;
    let mut prev_cq_vec = zero;

    let mut eq_vec = [zero; 4];
    let mut s_vec = [zero; 4];
    let mut src_vec = [zero; 4];
    let mut rho_vec = zero;
    let mut e_qmax_vec = zero;

    let n_loop = (width + 31) / 32;
    let stride = stride as usize;

    let mut first = true;
    // 2 lines per iteration
    let mut y = 0;
    while y < height {
        e_val_vec[n_loop as usize] = prev_e_val_vec;
        // lcxp[0] = (ui8)((rho[0] & 8) >> 3);
        let mut tmp = _mm512_and_epi32(prev_cx_val_vec, _mm512_set1_epi32(8));
        tmp = _mm512_srli_epi32::<3>(tmp);
        cx_val_vec[n_loop as usize] = tmp;

        prev_e_val_vec = zero;
        prev_cx_val_vec = zero;

        let row0 = y as usize * stride;
        // 32 samples per iteration
        for x in 0..n_loop as usize {
            // mask to stop loading unnecessary data
            let true_x = (x as i32) << 5;
            let mut mask32: u32 = 0xFFFFFFFF;
            let entries = true_x + 32 - width_in as i32;
            mask32 >>= if entries >= 0 { entries } else { 0 };
            let load_mask0 = mask32 as u16;
            let load_mask1 = (mask32 >> 16) as u16;

            let sp = row0 + x * 32;
            // t = sp[i]; (the masked lanes are the row's, inside buf)
            // SAFETY: the enabled lanes are samples below width of rows below height
            unsafe {
                let base = buf.as_ptr();
                src_vec[0] =
                    _mm512_maskz_loadu_epi32(load_mask0, base.wrapping_add(sp) as *const _);
                src_vec[2] =
                    _mm512_maskz_loadu_epi32(load_mask1, base.wrapping_add(sp + 16) as *const _);
                if y + 1 < height {
                    src_vec[1] = _mm512_maskz_loadu_epi32(
                        load_mask0,
                        base.wrapping_add(sp + stride) as *const _,
                    );
                    src_vec[3] = _mm512_maskz_loadu_epi32(
                        load_mask1,
                        base.wrapping_add(sp + 16 + stride) as *const _,
                    );
                } else {
                    src_vec[1] = zero;
                    src_vec[3] = zero;
                }
            }

            proc_pixel(
                &src_vec,
                p,
                &mut eq_vec,
                &mut s_vec,
                &mut rho_vec,
                &mut e_qmax_vec,
            );

            // max_e[(i + 1) % num] = ojph_max(lep[i + 1], lep[i + 2]) - 1;
            tmp = _mm512_permutexvar_epi32(right_shift, e_val_vec[x]);
            tmp = _mm512_mask_permutexvar_epi32(tmp, 0x8000, right_shift, e_val_vec[x + 1]);
            let mask = _mm512_cmpgt_epi32_mask(e_val_vec[x], tmp);
            let mut max_e_vec = _mm512_mask_mov_epi32(tmp, mask, e_val_vec[x]);
            max_e_vec = _mm512_sub_epi32(max_e_vec, one);

            // kappa[i] = (rho[i] & (rho[i] - 1)) ? ojph_max(1, max_e[i]) : 1;
            tmp = _mm512_max_epi32(max_e_vec, one);
            let mut tmp1 = _mm512_sub_epi32(rho_vec, one);
            tmp1 = _mm512_and_epi32(rho_vec, tmp1);
            let mask = _mm512_cmpneq_epi32_mask(tmp1, zero);
            let kappa_vec = _mm512_mask_mov_epi32(one, mask, tmp);

            // cq[1 - 16] = cq_vec; cq[0] = prev_cq_vec[0]
            tmp = if first {
                proc_cq1(rho_vec)
            } else {
                proc_cq2(x, &cx_val_vec, rho_vec, right_shift)
            };
            let cq_vec = _mm512_mask_permutexvar_epi32(prev_cq_vec, 0xFFFE, left_shift, tmp);
            prev_cq_vec = _mm512_mask_permutexvar_epi32(zero, 0x1, left_shift, tmp);

            update_lep(x, &mut prev_e_val_vec, &eq_vec, &mut e_val_vec, left_shift);
            update_lcxp(
                x,
                &mut prev_cx_val_vec,
                rho_vec,
                &mut cx_val_vec,
                left_shift,
            );

            // Uq[i] = ojph_max(e_qmax[i], kappa[i]); u_q[i] = Uq[i] - kappa[i];
            let uq_vec = _mm512_max_epi32(kappa_vec, e_qmax_vec);
            let u_q_vec = _mm512_sub_epi32(uq_vec, kappa_vec);

            let eps_vec = cal_eps_vec(&eq_vec, u_q_vec, e_qmax_vec);
            let vlc_tbl = if first { &tb.vlc_tbl0 } else { &tb.vlc_tbl1 };
            let mut tuple_vec = cal_tuple(cq_vec, rho_vec, eps_vec, vlc_tbl);
            let ignore_here = if (n_loop as usize - 1) == x {
                ignore
            } else {
                0
            };

            if first {
                proc_mel_encode1(&mut mel, cq_vec, rho_vec, u_q_vec, ignore_here, right_shift)?;
            } else {
                proc_mel_encode2(&mut mel, cq_vec, rho_vec, ignore_here)?;
            }

            proc_ms_encode(&mut ms, tuple_vec, uq_vec, rho_vec, &mut s_vec)?;

            // The tuple is scaled by 4 so that vlc_encode scales it by 2 only
            tuple_vec = _mm512_srli_epi32::<4>(tuple_vec);
            let tuple = lanes(tuple_vec);
            let u_q = lanes(u_q_vec);
            if first {
                proc_vlc_encode1(&mut vlc, &tuple, &u_q, ignore_here)?;
            } else {
                proc_vlc_encode2(&mut vlc, &tuple, &u_q, ignore_here)?;
            }
        }

        tmp = _mm512_permutexvar_epi32(right_shift, cx_val_vec[0]);
        tmp = _mm512_slli_epi32::<2>(tmp);
        prev_cq_vec = _mm512_maskz_add_epi32(0x1, tmp, cx_val_vec[0]);

        first = false;
        y += 2;
    }

    ms.terminate()?;
    terminate_mel_vlc(&mut mel, &mut vlc)?;

    // copy to elastic
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

/// [`ojph_encode_codeblock_avx512`] where the CPU has AVX-512F and AVX-512CD (upstream's
/// level check, `get_cpu_ext_level() >= X86_CPU_EXT_LEVEL_AVX512`, and the file's build
/// flags) and the buffer holds the codeblock; `None` otherwise.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_codeblock_avx512(
    buf: &[u32],
    missing_msbs: u32,
    num_passes: u32,
    width: u32,
    height: u32,
    stride: u32,
    lengths: &mut [u32; 2],
    coded: &mut Option<crate::mem::Pooled<u8>>,
) -> Option<OjphResult<()>> {
    if !(std::arch::is_x86_feature_detected!("avx512f")
        && std::arch::is_x86_feature_detected!("avx512cd"))
    {
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
    // SAFETY: the CPU has AVX-512F and AVX-512CD, and the buffer holds the codeblock
    Some(unsafe {
        ojph_encode_codeblock_avx512(
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
