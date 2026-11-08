// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_encoder.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:1-1523 @ OpenJPH 0.32.0) and
//! `ojph_block_encoder.h` (external/OpenJPH/src/core/coding/ojph_block_encoder.h:1-88
//! @ OpenJPH 0.32.0): the HT block encoder, the cleanup pass of a codeblock of 32- or 64-bit
//! samples. Upstream's AVX2 and AVX-512 encoders, which it chooses on x86-64, write the same
//! bytes (checked through Core's HTJ2K encoder against the wheel).

use std::sync::OnceLock;

use crate::arch::{count_leading_zeros, count_leading_zeros64, population_count};
use crate::bitbuffer_read::{PREFIX_BUF_SIZE, SUFFIX_BUF_SIZE};
use crate::block_common::{TBL0, TBL1};
use crate::message::{OjphResult, error};

const FILE: &str = "ojph_block_encoder.cpp";

/// `uvlc_tbl_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:70-72
/// @ OpenJPH 0.32.0).
#[derive(Debug, Clone, Copy, Default)]
struct UvlcEntry {
    pre: u8,
    pre_len: u8,
    suf: u8,
    suf_len: u8,
    ext: u8,
    ext_len: u8,
}

/// `num_uvlc_entries`.
const NUM_UVLC_ENTRIES: usize = 75;

/// The encoder's tables: `vlc_tbl0`, `vlc_tbl1` (index `(c_q << 8) + (rho << 4) + eps`, data
/// `(cwd << 8) + (cwd_len << 4) + eps`) and `uvlc_tbl`.
struct EncoderTables {
    vlc_tbl0: [u16; 2048],
    vlc_tbl1: [u16; 2048],
    uvlc_tbl: [UvlcEntry; NUM_UVLC_ENTRIES],
}

/// One of `vlc_init_tables`' two passes
/// (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:76-193 @ OpenJPH 0.32.0).
fn vlc_init_table(tgt_tbl: &mut [u16; 2048], src_tbl: &[[u8; 7]]) {
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
        *tgt = ((u16::from(e[5]) << 8) + (u16::from(e[6]) << 4)) + u16::from(e[3]);
    }
}

/// Port of `uvlc_init_tables` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:196-255
/// @ OpenJPH 0.32.0).
fn uvlc_init_tables() -> [UvlcEntry; NUM_UVLC_ENTRIES] {
    let mut t = [UvlcEntry::default(); NUM_UVLC_ENTRIES];
    let set = |pre, pre_len, suf, suf_len, ext, ext_len| UvlcEntry {
        pre,
        pre_len,
        suf,
        suf_len,
        ext,
        ext_len,
    };
    // code goes from 0 to 31, extension and 32 are not supported here
    t[0] = set(0, 0, 0, 0, 0, 0);
    t[1] = set(1, 1, 0, 0, 0, 0);
    t[2] = set(2, 2, 0, 0, 0, 0);
    t[3] = set(4, 3, 0, 1, 0, 0);
    t[4] = set(4, 3, 1, 1, 0, 0);
    for (i, e) in t.iter_mut().enumerate().take(33).skip(5) {
        *e = set(0, 3, (i - 5) as u8, 5, 0, 0);
    }
    for (i, e) in t.iter_mut().enumerate().skip(33) {
        *e = set(0, 3, (28 + (i - 33) % 4) as u8, 5, ((i - 33) / 4) as u8, 4);
    }
    t
}

/// Port of `initialize_block_encoder_tables`
/// (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:258-268 @ OpenJPH 0.32.0).
fn tables() -> &'static EncoderTables {
    static T: OnceLock<EncoderTables> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = EncoderTables {
            vlc_tbl0: [0; 2048],
            vlc_tbl1: [0; 2048],
            uvlc_tbl: uvlc_init_tables(),
        };
        vlc_init_table(&mut t.vlc_tbl0, &TBL0);
        vlc_init_table(&mut t.vlc_tbl1, &TBL1);
        t
    })
}

/// `ojph_max (a, b)` and `ojph_min (a, b)` of `int`s.
#[inline]
fn imax(a: i32, b: i32) -> i32 {
    if a > b { a } else { b }
}
#[inline]
fn imin(a: i32, b: i32) -> i32 {
    if a < b { a } else { b }
}

/// `MEL exponent`.
const MEL_EXP: [i32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];

/// Port of `mel_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:273-285
/// @ OpenJPH 0.32.0), with `mel_init` (lines 288-299).
struct Mel {
    buf: [u8; MEL_SIZE],
    pos: u32,
    remaining_bits: i32,
    tmp: i32,
    run: i32,
    k: i32,
    threshold: i32,
}

/// `mel_size` and `vlc_size`: the parts of upstream's `mel_vlc_buf` of 3072 bytes.
const MEL_SIZE: usize = 192;
const VLC_SIZE: usize = 3072 - MEL_SIZE;

impl Mel {
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

    /// Port of `mel_emit_bit` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:302-317
    /// @ OpenJPH 0.32.0).
    fn emit_bit(&mut self, v: i32) -> OjphResult<()> {
        self.tmp = (self.tmp << 1) + v;
        self.remaining_bits -= 1;
        if self.remaining_bits == 0 {
            if self.pos as usize >= MEL_SIZE {
                return Err(error(
                    0x00020001,
                    FILE,
                    311,
                    "mel encoder's buffer is full",
                    &[],
                ));
            }
            self.buf[self.pos as usize] = self.tmp as u8;
            self.pos += 1;
            self.remaining_bits = if self.tmp == 0xFF { 7 } else { 8 };
            self.tmp = 0;
        }
        Ok(())
    }

    /// Port of `mel_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:320-346
    /// @ OpenJPH 0.32.0).
    fn encode(&mut self, bit: bool) -> OjphResult<()> {
        if !bit {
            self.run += 1;
            if self.run >= self.threshold {
                self.emit_bit(1)?;
                self.run = 0;
                self.k = imin(12, self.k + 1);
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
            self.k = imax(0, self.k - 1);
            self.threshold = 1 << MEL_EXP[self.k as usize];
        }
        Ok(())
    }
}

/// Port of `vlc_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:352-361
/// @ OpenJPH 0.32.0), with `vlc_init` (lines 364-375): written backwards from the buffer's
/// last byte, `buf - pos` being `buf[VLC_SIZE - 1 - pos]`.
struct Vlc {
    buf: [u8; VLC_SIZE],
    pos: u32,
    used_bits: i32,
    tmp: i32,
    last_greater_than_8f: bool,
}

impl Vlc {
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

    fn put(&mut self, v: u8) {
        self.buf[VLC_SIZE - 1 - self.pos as usize] = v;
    }

    /// Port of `vlc_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:378-409
    /// @ OpenJPH 0.32.0).
    fn encode(&mut self, mut cwd: i32, mut cwd_len: i32) -> OjphResult<()> {
        while cwd_len > 0 {
            if self.pos as usize >= VLC_SIZE {
                return Err(error(
                    0x00020002,
                    FILE,
                    384,
                    "vlc encoder's buffer is full",
                    &[],
                ));
            }
            let mut avail_bits = 8 - i32::from(self.last_greater_than_8f) - self.used_bits;
            let t = imin(avail_bits, cwd_len);
            self.tmp |= (cwd & ((1 << t) - 1)) << self.used_bits;
            self.used_bits += t;
            avail_bits -= t;
            cwd_len -= t;
            cwd >>= t;
            if avail_bits == 0 {
                if self.last_greater_than_8f && self.tmp != 0x7F {
                    self.last_greater_than_8f = false;
                    continue; // one empty bit remaining
                }
                self.put(self.tmp as u8);
                self.pos += 1;
                self.last_greater_than_8f = self.tmp > 0x8F;
                self.tmp = 0;
                self.used_bits = 0;
            }
        }
        Ok(())
    }
}

/// Port of `terminate_mel_vlc` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:412-441
/// @ OpenJPH 0.32.0).
fn terminate_mel_vlc(mel: &mut Mel, vlc: &mut Vlc) -> OjphResult<()> {
    if mel.run > 0 {
        mel.emit_bit(1)?;
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
            425,
            "mel encoder's buffer is full",
            &[],
        ));
    }
    let fuse = mel.tmp | vlc.tmp;
    if ((((fuse ^ mel.tmp) & mel_mask) | ((fuse ^ vlc.tmp) & vlc_mask)) == 0)
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
                436,
                "vlc encoder's buffer is full",
                &[],
            ));
        }
        mel.buf[mel.pos as usize] = mel.tmp as u8; // mel.tmp cannot be 0xFF
        mel.pos += 1;
        vlc.put(vlc.tmp as u8);
        vlc.pos += 1;
    }
    Ok(())
}

/// Port of `ms_struct` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:446-455
/// @ OpenJPH 0.32.0), with `ms_init` (lines 458-467).
struct Ms {
    buf: Vec<u8>,
    pos: u32,
    max_bits: i32,
    used_bits: i32,
    tmp: u32,
}

impl Ms {
    fn new(size: usize) -> Self {
        Ms {
            buf: vec![0; size],
            pos: 0,
            max_bits: 8,
            used_bits: 0,
            tmp: 0,
        }
    }

    /// Port of `ms_encode` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:470-490
    /// @ OpenJPH 0.32.0) and `ms_encode64` (lines 493-513).
    fn encode(&mut self, mut cwd: u64, mut cwd_len: i32, line: u32) -> OjphResult<()> {
        while cwd_len > 0 {
            if self.pos as usize >= self.buf.len() {
                return Err(error(
                    0x00020005,
                    FILE,
                    line,
                    "magnitude sign encoder's buffer is full",
                    &[],
                ));
            }
            let t = imin(self.max_bits - self.used_bits, cwd_len);
            self.tmp |= ((cwd & ((1u64 << t) - 1)) << self.used_bits) as u32;
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

    /// Port of `ms_terminate` (external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:516-533
    /// @ OpenJPH 0.32.0).
    fn terminate(&mut self) -> OjphResult<()> {
        if self.used_bits != 0 {
            let t = self.max_bits - self.used_bits; // unused bits
            self.tmp |= (0xFF & ((1u32 << t) - 1)) << self.used_bits;
            self.used_bits += t;
            if self.tmp != 0xFF {
                if self.pos as usize >= self.buf.len() {
                    return Err(error(
                        0x00020006,
                        FILE,
                        527,
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

/// The coded bytes of a codeblock, as `coded_lists` holds them: `PREFIX_BUF_SIZE` bytes, the
/// MagSgn, MEL and VLC segments with the interface locator word, then `SUFFIX_BUF_SIZE` bytes
/// (the layout the decoder's buffers have, so either side reads `PREFIX_BUF_SIZE` on).
fn assemble(mel: &Mel, vlc: &Vlc, ms: &Ms, lengths: &mut [u32; 2]) -> Vec<u8> {
    let (mp, vp, sp) = (mel.pos as usize, vlc.pos as usize, ms.pos as usize);
    lengths[0] = (mp + vp + sp) as u32;
    let n = lengths[0] as usize;
    let mut out = vec![0u8; PREFIX_BUF_SIZE + n + SUFFIX_BUF_SIZE];
    let c = &mut out[PREFIX_BUF_SIZE..PREFIX_BUF_SIZE + n];
    c[..sp].copy_from_slice(&ms.buf[..sp]);
    c[sp..sp + mp].copy_from_slice(&mel.buf[..mp]);
    c[sp + mp..].copy_from_slice(&vlc.buf[VLC_SIZE - vp..]);

    // put in the interface locator word
    let num_bytes = (mp + vp) as u32;
    c[n - 1] = (num_bytes >> 4) as u8;
    c[n - 2] &= 0xF0;
    c[n - 2] |= (num_bytes & 0xF) as u8;
    out
}

/// The sample of a quad: `None` when it is insignificant, else `(e_q, s)`, its exponent and
/// its `v_n = 2 (mu_p - 1) + s_n`.
macro_rules! quad_sample {
    ($t:ty, $bits:expr, $clz:ident) => {
        #[inline]
        fn sample(t: $t, p: u32) -> Option<(i32, $t)> {
            let mut val = t.wrapping_add(t); // multiply by 2 and get rid of sign
            val = val.wrapping_shr(p); // 2 \mu_p + x
            val &= !1; // 2 \mu_p
            if val == 0 {
                return None;
            }
            val -= 1;
            let e_q = $bits - $clz(val) as i32; // 2\mu_p - 1
            val -= 1;
            Some((e_q, val + (t >> ($bits - 1)))) // v_n = 2(\mu_p-1) + s_n
        }
    };
}

/// `ojph_encode_codeblock32` and `ojph_encode_codeblock64`, which differ in their sample
/// width, their MagSgn buffer, and the 64-bit one's UVLC extensions.
macro_rules! encode_codeblock {
    ($name:ident, $t:ty, $bits:expr, $clz:ident, $ms_size:expr, $ext:expr, $ms_line:expr) => {
        #[allow(clippy::too_many_arguments)]
        pub(crate) fn $name(
            buf: &[$t],
            missing_msbs: u32,
            num_passes: u32,
            width: u32,
            height: u32,
            stride: u32,
            lengths: &mut [u32; 2],
            coded: &mut Option<Vec<u8>>,
        ) -> OjphResult<()> {
            quad_sample!($t, $bits, $clz);
            debug_assert!(num_passes == 1);
            let tb = tables();
            let mut mel = Mel::new();
            let mut vlc = Vlc::new();
            let mut ms = Ms::new($ms_size);

            let p = ($bits as u32 - 2).wrapping_sub(missing_msbs);
            let stride = stride as usize;
            let mask =
                |m: i32| -> u64 { ((1 as $t).wrapping_shl(m as u32)).wrapping_sub(1) as u64 };

            // e_val: the E values of a line (the highest set bit), cx_val: the context values;
            // each byte for 2 samples. Upstream's arrays have 513 bytes; a codeblock 1024
            // wide reaches the 514th (docs/improvements.md, U-19), which the port has, zeroed.
            let mut e_val = [0u8; 515];
            let mut cx_val = [0u8; 515];
            let (mut lep, mut lcxp) = (0usize, 0usize);
            e_val[0] = 0;
            cx_val[0] = 0;

            // the samples of the four of a quad, at sp, sp + stride, sp + 1, sp + 1 + stride
            let read = |sp: usize, below: bool| -> $t { if below { buf[sp + stride] } else { 0 } };

            // initial row of quads
            let mut e_qmax = [0i32; 2];
            let mut e_q = [0i32; 8];
            let mut rho = [0i32; 2];
            let mut c_q0 = 0i32;
            let mut s: [$t; 8] = [0; 8];
            let mut sp = 0usize;
            let below = height > 1;
            let mut x = 0u32;
            while x < width {
                // prepare two quads
                if let Some((e, v)) = sample(buf[sp], p) {
                    rho[0] = 1;
                    e_q[0] = e;
                    e_qmax[0] = e;
                    s[0] = v;
                }
                if let Some((e, v)) = sample(read(sp, below), p) {
                    rho[0] += 2;
                    e_q[1] = e;
                    e_qmax[0] = imax(e_qmax[0], e);
                    s[1] = v;
                }
                sp += 1;
                if x + 1 < width {
                    if let Some((e, v)) = sample(buf[sp], p) {
                        rho[0] += 4;
                        e_q[2] = e;
                        e_qmax[0] = imax(e_qmax[0], e);
                        s[2] = v;
                    }
                    if let Some((e, v)) = sample(read(sp, below), p) {
                        rho[0] += 8;
                        e_q[3] = e;
                        e_qmax[0] = imax(e_qmax[0], e);
                        s[3] = v;
                    }
                    sp += 1;
                }

                let uq0 = imax(e_qmax[0], 1); // kappa_q = 1
                let u_q0 = uq0 - 1;
                let mut u_q1 = 0; // kappa_q = 1

                let mut eps0 = 0;
                if u_q0 > 0 {
                    eps0 |= i32::from(e_q[0] == e_qmax[0]);
                    eps0 |= i32::from(e_q[1] == e_qmax[0]) << 1;
                    eps0 |= i32::from(e_q[2] == e_qmax[0]) << 2;
                    eps0 |= i32::from(e_q[3] == e_qmax[0]) << 3;
                }
                e_val[lep] = e_val[lep].max(e_q[1] as u8);
                lep += 1;
                e_val[lep] = e_q[3] as u8;
                cx_val[lcxp] |= ((rho[0] & 2) >> 1) as u8;
                lcxp += 1;
                cx_val[lcxp] = ((rho[0] & 8) >> 3) as u8;

                let tuple0 = i32::from(tb.vlc_tbl0[((c_q0 << 8) + (rho[0] << 4) + eps0) as usize]);
                vlc.encode(tuple0 >> 8, (tuple0 >> 4) & 7)?;

                if c_q0 == 0 {
                    mel.encode(rho[0] != 0)?;
                }

                for i in 0..4 {
                    let m = if (rho[0] & (1 << i)) != 0 {
                        uq0 - ((tuple0 & (1 << i)) >> i)
                    } else {
                        0
                    };
                    ms.encode(s[i] as u64 & mask(m), m, $ms_line)?;
                }

                if x + 2 < width {
                    if let Some((e, v)) = sample(buf[sp], p) {
                        rho[1] = 1;
                        e_q[4] = e;
                        e_qmax[1] = e;
                        s[4] = v;
                    }
                    if let Some((e, v)) = sample(read(sp, below), p) {
                        rho[1] += 2;
                        e_q[5] = e;
                        e_qmax[1] = imax(e_qmax[1], e);
                        s[5] = v;
                    }
                    sp += 1;
                    if x + 3 < width {
                        if let Some((e, v)) = sample(buf[sp], p) {
                            rho[1] += 4;
                            e_q[6] = e;
                            e_qmax[1] = imax(e_qmax[1], e);
                            s[6] = v;
                        }
                        if let Some((e, v)) = sample(read(sp, below), p) {
                            rho[1] += 8;
                            e_q[7] = e;
                            e_qmax[1] = imax(e_qmax[1], e);
                            s[7] = v;
                        }
                        sp += 1;
                    }

                    let c_q1 = (rho[0] >> 1) | (rho[0] & 1);
                    let uq1 = imax(e_qmax[1], 1); // kappa_q = 1
                    u_q1 = uq1 - 1; // kappa_q = 1

                    let mut eps1 = 0;
                    if u_q1 > 0 {
                        eps1 |= i32::from(e_q[4] == e_qmax[1]);
                        eps1 |= i32::from(e_q[5] == e_qmax[1]) << 1;
                        eps1 |= i32::from(e_q[6] == e_qmax[1]) << 2;
                        eps1 |= i32::from(e_q[7] == e_qmax[1]) << 3;
                    }
                    e_val[lep] = e_val[lep].max(e_q[5] as u8);
                    lep += 1;
                    e_val[lep] = e_q[7] as u8;
                    cx_val[lcxp] |= ((rho[1] & 2) >> 1) as u8;
                    lcxp += 1;
                    cx_val[lcxp] = ((rho[1] & 8) >> 3) as u8;
                    let tuple1 =
                        i32::from(tb.vlc_tbl0[((c_q1 << 8) + (rho[1] << 4) + eps1) as usize]);
                    vlc.encode(tuple1 >> 8, (tuple1 >> 4) & 7)?;

                    if c_q1 == 0 {
                        mel.encode(rho[1] != 0)?;
                    }

                    for i in 0..4 {
                        let m = if (rho[1] & (1 << i)) != 0 {
                            uq1 - ((tuple1 & (1 << i)) >> i)
                        } else {
                            0
                        };
                        ms.encode(s[4 + i] as u64 & mask(m), m, $ms_line)?;
                    }
                }

                if u_q0 > 0 && u_q1 > 0 {
                    mel.encode(imin(u_q0, u_q1) > 2)?;
                }

                let u = &tb.uvlc_tbl;
                let enc =
                    |vlc: &mut Vlc, cwd: u8, len: u8| vlc.encode(i32::from(cwd), i32::from(len));
                if u_q0 > 2 && u_q1 > 2 {
                    let (a, b) = (u[(u_q0 - 2) as usize], u[(u_q1 - 2) as usize]);
                    enc(&mut vlc, a.pre, a.pre_len)?;
                    enc(&mut vlc, b.pre, b.pre_len)?;
                    enc(&mut vlc, a.suf, a.suf_len)?;
                    enc(&mut vlc, b.suf, b.suf_len)?;
                    if $ext {
                        enc(&mut vlc, a.ext, a.ext_len)?;
                        enc(&mut vlc, b.ext, b.ext_len)?;
                    }
                } else if u_q0 > 2 && u_q1 > 0 {
                    let a = u[u_q0 as usize];
                    enc(&mut vlc, a.pre, a.pre_len)?;
                    vlc.encode(u_q1 - 1, 1)?;
                    enc(&mut vlc, a.suf, a.suf_len)?;
                    if $ext {
                        enc(&mut vlc, a.ext, a.ext_len)?;
                    }
                } else {
                    let (a, b) = (u[u_q0 as usize], u[u_q1 as usize]);
                    enc(&mut vlc, a.pre, a.pre_len)?;
                    enc(&mut vlc, b.pre, b.pre_len)?;
                    enc(&mut vlc, a.suf, a.suf_len)?;
                    enc(&mut vlc, b.suf, b.suf_len)?;
                    if $ext {
                        enc(&mut vlc, a.ext, a.ext_len)?;
                        enc(&mut vlc, b.ext, b.ext_len)?;
                    }
                }

                // prepare for next iteration
                c_q0 = (rho[1] >> 1) | (rho[1] & 1);
                s = [0; 8];
                e_q = [0; 8];
                rho = [0; 2];
                e_qmax = [0; 2];
                x += 4;
            }

            e_val[lep + 1] = 0;

            let mut y = 2u32;
            while y < height {
                lep = 0;
                let mut max_e = imax(i32::from(e_val[0]), i32::from(e_val[1])) - 1;
                e_val[0] = 0;
                lcxp = 0;
                c_q0 = i32::from(cx_val[0]) + (i32::from(cx_val[1]) << 2);
                cx_val[0] = 0;

                let below = y + 1 < height;
                sp = y as usize * stride;
                let mut x = 0u32;
                while x < width {
                    // prepare two quads
                    if let Some((e, v)) = sample(buf[sp], p) {
                        rho[0] = 1;
                        e_q[0] = e;
                        e_qmax[0] = e;
                        s[0] = v;
                    }
                    if let Some((e, v)) = sample(read(sp, below), p) {
                        rho[0] += 2;
                        e_q[1] = e;
                        e_qmax[0] = imax(e_qmax[0], e);
                        s[1] = v;
                    }
                    sp += 1;
                    if x + 1 < width {
                        if let Some((e, v)) = sample(buf[sp], p) {
                            rho[0] += 4;
                            e_q[2] = e;
                            e_qmax[0] = imax(e_qmax[0], e);
                            s[2] = v;
                        }
                        if let Some((e, v)) = sample(read(sp, below), p) {
                            rho[0] += 8;
                            e_q[3] = e;
                            e_qmax[0] = imax(e_qmax[0], e);
                            s[3] = v;
                        }
                        sp += 1;
                    }

                    let mut kappa = if (rho[0] & (rho[0] - 1)) != 0 {
                        imax(1, max_e)
                    } else {
                        1
                    };
                    let uq0 = imax(e_qmax[0], kappa);
                    let u_q0 = uq0 - kappa;
                    let mut u_q1 = 0;

                    let mut eps0 = 0;
                    if u_q0 > 0 {
                        eps0 |= i32::from(e_q[0] == e_qmax[0]);
                        eps0 |= i32::from(e_q[1] == e_qmax[0]) << 1;
                        eps0 |= i32::from(e_q[2] == e_qmax[0]) << 2;
                        eps0 |= i32::from(e_q[3] == e_qmax[0]) << 3;
                    }
                    e_val[lep] = e_val[lep].max(e_q[1] as u8);
                    lep += 1;
                    max_e = imax(i32::from(e_val[lep]), i32::from(e_val[lep + 1])) - 1;
                    e_val[lep] = e_q[3] as u8;
                    cx_val[lcxp] |= ((rho[0] & 2) >> 1) as u8;
                    lcxp += 1;
                    let mut c_q1 = i32::from(cx_val[lcxp]) + (i32::from(cx_val[lcxp + 1]) << 2);
                    cx_val[lcxp] = ((rho[0] & 8) >> 3) as u8;
                    let tuple0 =
                        i32::from(tb.vlc_tbl1[((c_q0 << 8) + (rho[0] << 4) + eps0) as usize]);
                    vlc.encode(tuple0 >> 8, (tuple0 >> 4) & 7)?;

                    if c_q0 == 0 {
                        mel.encode(rho[0] != 0)?;
                    }

                    for i in 0..4 {
                        let m = if (rho[0] & (1 << i)) != 0 {
                            uq0 - ((tuple0 & (1 << i)) >> i)
                        } else {
                            0
                        };
                        ms.encode(s[i] as u64 & mask(m), m, $ms_line)?;
                    }

                    if x + 2 < width {
                        if let Some((e, v)) = sample(buf[sp], p) {
                            rho[1] = 1;
                            e_q[4] = e;
                            e_qmax[1] = e;
                            s[4] = v;
                        }
                        if let Some((e, v)) = sample(read(sp, below), p) {
                            rho[1] += 2;
                            e_q[5] = e;
                            e_qmax[1] = imax(e_qmax[1], e);
                            s[5] = v;
                        }
                        sp += 1;
                        if x + 3 < width {
                            if let Some((e, v)) = sample(buf[sp], p) {
                                rho[1] += 4;
                                e_q[6] = e;
                                e_qmax[1] = imax(e_qmax[1], e);
                                s[6] = v;
                            }
                            if let Some((e, v)) = sample(read(sp, below), p) {
                                rho[1] += 8;
                                e_q[7] = e;
                                e_qmax[1] = imax(e_qmax[1], e);
                                s[7] = v;
                            }
                            sp += 1;
                        }

                        kappa = if (rho[1] & (rho[1] - 1)) != 0 {
                            imax(1, max_e)
                        } else {
                            1
                        };
                        c_q1 |= ((rho[0] & 4) >> 1) | ((rho[0] & 8) >> 2);
                        let uq1 = imax(e_qmax[1], kappa);
                        u_q1 = uq1 - kappa;

                        let mut eps1 = 0;
                        if u_q1 > 0 {
                            eps1 |= i32::from(e_q[4] == e_qmax[1]);
                            eps1 |= i32::from(e_q[5] == e_qmax[1]) << 1;
                            eps1 |= i32::from(e_q[6] == e_qmax[1]) << 2;
                            eps1 |= i32::from(e_q[7] == e_qmax[1]) << 3;
                        }
                        e_val[lep] = e_val[lep].max(e_q[5] as u8);
                        lep += 1;
                        max_e = imax(i32::from(e_val[lep]), i32::from(e_val[lep + 1])) - 1;
                        e_val[lep] = e_q[7] as u8;
                        cx_val[lcxp] |= ((rho[1] & 2) >> 1) as u8;
                        lcxp += 1;
                        c_q0 = i32::from(cx_val[lcxp]) + (i32::from(cx_val[lcxp + 1]) << 2);
                        cx_val[lcxp] = ((rho[1] & 8) >> 3) as u8;
                        let tuple1 =
                            i32::from(tb.vlc_tbl1[((c_q1 << 8) + (rho[1] << 4) + eps1) as usize]);
                        vlc.encode(tuple1 >> 8, (tuple1 >> 4) & 7)?;

                        if c_q1 == 0 {
                            mel.encode(rho[1] != 0)?;
                        }

                        for i in 0..4 {
                            let m = if (rho[1] & (1 << i)) != 0 {
                                uq1 - ((tuple1 & (1 << i)) >> i)
                            } else {
                                0
                            };
                            ms.encode(s[4 + i] as u64 & mask(m), m, $ms_line)?;
                        }
                    }

                    let u = &tb.uvlc_tbl;
                    let (a, b) = (u[u_q0 as usize], u[u_q1 as usize]);
                    vlc.encode(i32::from(a.pre), i32::from(a.pre_len))?;
                    vlc.encode(i32::from(b.pre), i32::from(b.pre_len))?;
                    vlc.encode(i32::from(a.suf), i32::from(a.suf_len))?;
                    vlc.encode(i32::from(b.suf), i32::from(b.suf_len))?;
                    if $ext {
                        vlc.encode(i32::from(a.ext), i32::from(a.ext_len))?;
                        vlc.encode(i32::from(b.ext), i32::from(b.ext_len))?;
                    }

                    // prepare for next iteration
                    c_q0 |= ((rho[1] & 4) >> 1) | ((rho[1] & 8) >> 2);
                    s = [0; 8];
                    e_q = [0; 8];
                    rho = [0; 2];
                    e_qmax = [0; 2];
                    x += 4;
                }
                y += 2;
            }

            terminate_mel_vlc(&mut mel, &mut vlc)?;
            ms.terminate()?;

            // copy to elastic
            *coded = Some(assemble(&mel, &vlc, &ms, lengths));
            Ok(())
        }
    };
}

encode_codeblock!(
    ojph_encode_codeblock32,
    u32,
    32,
    count_leading_zeros,
    (16384 * 16 + 14) / 15,
    false,
    476
);
encode_codeblock!(
    ojph_encode_codeblock64,
    u64,
    64,
    count_leading_zeros64,
    (22528 * 16 + 14) / 15,
    true,
    499
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_decoder32::ojph_decode_codeblock32;
    use crate::block_decoder64::ojph_decode_codeblock64;
    use crate::codestream_gen::{
        rev_tx_from_cb32, rev_tx_from_cb64, rev_tx_to_cb32, rev_tx_to_cb64,
    };

    /// A deterministic sequence of samples of `bits` bits of magnitude, many of them zero.
    fn samples(n: usize, bits: u32, seed: u64) -> Vec<i64> {
        let mut x = seed;
        (0..n)
            .map(|i| {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let r = (x >> 33) as i64;
                if i % 5 == 0 {
                    0
                } else {
                    let m = r & ((1i64 << bits) - 1);
                    if r & (1 << 40) != 0 { -m } else { m }
                }
            })
            .collect()
    }

    /// The decoder reconstructs what the encoder coded, for codeblocks of many shapes, in 32
    /// and 64 bits.
    #[test]
    fn codeblocks_decode_as_they_were_encoded() {
        for (w, h) in [
            (1, 1),
            (3, 1),
            (5, 2),
            (7, 3),
            (64, 64),
            (128, 32),
            (33, 17),
            (4, 9),
        ] {
            for k_max in [3u32, 12, 20] {
                let stride = (w + 15) & !15;
                let n = (stride * h) as usize;
                let vals = samples(n, k_max, u64::from(w * 1000 + h * 10 + k_max));

                // 32 bits
                let sp: Vec<i32> = vals.iter().map(|&v| v as i32).collect();
                let mut cb = vec![0u32; n];
                let mut mv = 0;
                for y in 0..h as usize {
                    let r = y * stride as usize;
                    rev_tx_to_cb32(&sp[r..], &mut cb[r..], k_max, w, &mut mv);
                }
                if mv >= 1u32 << (31 - k_max) {
                    let mut lengths = [0u32; 2];
                    let mut coded = None;
                    ojph_encode_codeblock32(
                        &cb,
                        k_max - 1,
                        1,
                        w,
                        h,
                        stride,
                        &mut lengths,
                        &mut coded,
                    )
                    .expect("encodes");
                    let coded = coded.expect("coded");
                    let mut dec = vec![0u32; n + stride as usize];
                    assert!(ojph_decode_codeblock32(
                        &coded[PREFIX_BUF_SIZE..],
                        &mut dec,
                        k_max - 1,
                        1,
                        lengths[0],
                        0,
                        w,
                        h,
                        stride,
                        false
                    ));
                    for y in 0..h as usize {
                        let r = y * stride as usize;
                        let mut back = vec![0i32; w as usize];
                        rev_tx_from_cb32(&dec[r..], &mut back, k_max, w);
                        assert_eq!(
                            &back[..],
                            &sp[r..r + w as usize],
                            "32-bit {w}x{h} K_max {k_max} row {y}"
                        );
                    }
                }

                // 64 bits, magnitudes shifted into the upper half
                let k64 = k_max + 30;
                let sp: Vec<i64> = vals.iter().map(|&v| v << 30).collect();
                let mut cb = vec![0u64; n];
                let mut mv = 0;
                for y in 0..h as usize {
                    let r = y * stride as usize;
                    rev_tx_to_cb64(&sp[r..], &mut cb[r..], k64, w, &mut mv);
                }
                if mv >= 1u64 << (63 - k64) {
                    let mut lengths = [0u32; 2];
                    let mut coded = None;
                    ojph_encode_codeblock64(
                        &cb,
                        k64 - 1,
                        1,
                        w,
                        h,
                        stride,
                        &mut lengths,
                        &mut coded,
                    )
                    .expect("encodes");
                    let coded = coded.expect("coded");
                    let mut dec = vec![0u64; n + stride as usize];
                    assert!(ojph_decode_codeblock64(
                        &coded[PREFIX_BUF_SIZE..],
                        &mut dec,
                        k64 - 1,
                        1,
                        lengths[0],
                        0,
                        w,
                        h,
                        stride,
                        false
                    ));
                    for y in 0..h as usize {
                        let r = y * stride as usize;
                        let mut back = vec![0i64; w as usize];
                        rev_tx_from_cb64(&dec[r..], &mut back, k64, w);
                        assert_eq!(
                            &back[..],
                            &sp[r..r + w as usize],
                            "64-bit {w}x{h} K_max {k64} row {y}"
                        );
                    }
                }
            }
        }
    }
}
