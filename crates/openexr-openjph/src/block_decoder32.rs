// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_block_decoder32.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:1-1616 @ OpenJPH 0.32.0): the
//! HT block decoder for samples of up to 31 magnitude bits: the cleanup pass (MEL, VLC and
//! MagSgn streams), then the significance propagation and magnitude refinement passes.
//!
//! The readers start with as many bytes as bring their pointer to a 4-byte boundary, then
//! read 4 bytes at a time, as upstream's do; the bits they produce do not depend on where
//! the boundary falls.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{count_leading_zeros, population_count};
use crate::block_common::tables;
use crate::message::{Arg, warn};

const FILE: &str = "ojph_block_decoder32.cpp";

/// `load_le_ui32` of `d` at `i`.
#[inline]
pub(crate) fn load_le_ui32(d: &[u8], i: isize) -> u32 {
    let i = i as usize;
    u32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
}

/// The address of `d[i]` modulo 4 (`intptr_t (p) & 0x3`).
#[inline]
pub(crate) fn align4(d: &[u8], i: isize) -> isize {
    ((d.as_ptr() as usize).wrapping_add(i as usize) & 0x3) as isize
}

/// Port of `dec_mel_st` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:63-77
/// @ OpenJPH 0.32.0): the MEL decoder's state.
pub(crate) struct DecMel<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) data: isize,
    pub(crate) tmp: u64,
    pub(crate) bits: i32,
    pub(crate) size: i32,
    pub(crate) unstuff: bool,
    pub(crate) k: i32,
    pub(crate) num_runs: i32,
    pub(crate) runs: u64,
}

impl<'a> DecMel<'a> {
    /// Port of `mel_read` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:93-152
    /// @ OpenJPH 0.32.0): up to 32 more bits, unstuffed.
    fn read(&mut self) {
        if self.bits > 32 {
            // there are enough bits in the tmp variable
            return;
        }
        let mut val: u32 = 0xFFFFFFFF; // feed in 0xFF if buffer is exhausted
        if self.size > 4 {
            // if there is data in the MEL segment
            val = load_le_ui32(self.buf, self.data); // read 32 bits from MEL data
            self.data += 4;
            self.size -= 4;
        } else if self.size > 0 {
            // 4 or less
            let mut i = 0;
            while self.size > 1 {
                let v = u32::from(self.buf[self.data as usize]); // read one byte at a time
                self.data += 1;
                let m = !(0xFFu32 << i); // mask of location
                val = (val & m) | (v << i); // put one byte in its correct location
                self.size -= 1;
                i += 8;
            }
            // size equal to 1
            let mut v = u32::from(self.buf[self.data as usize]); // the one before the last is different
            self.data += 1;
            v |= 0xF; // MEL and VLC segments can overlap
            let m = !(0xFFu32 << i);
            val = (val & m) | (v << i);
            self.size -= 1;
        }

        // next we unstuff them before adding them to the buffer
        let mut bits = 32 - i32::from(self.unstuff); // number of bits in val, subtract 1 if
        // the previously read byte requires unstuffing
        let mut t = val & 0xFF;
        let mut unstuff = (val & 0xFF) == 0xFF; // true if we need unstuffing
        bits -= i32::from(unstuff); // there is one less bit in t if unstuffing is needed
        t <<= 8 - u32::from(unstuff); // move up to make room for the next byte

        t |= (val >> 8) & 0xFF;
        unstuff = ((val >> 8) & 0xFF) == 0xFF;
        bits -= i32::from(unstuff);
        t <<= 8 - u32::from(unstuff);

        t |= (val >> 16) & 0xFF;
        unstuff = ((val >> 16) & 0xFF) == 0xFF;
        bits -= i32::from(unstuff);
        t <<= 8 - u32::from(unstuff);

        t |= (val >> 24) & 0xFF;
        self.unstuff = ((val >> 24) & 0xFF) == 0xFF;

        // move t to tmp, and push the result all the way up, so we read from the MSB
        self.tmp |= u64::from(t) << (64 - bits - self.bits);
        self.bits += bits; // increment the number of bits in tmp
    }

    /// Port of `mel_decode` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:170-208
    /// @ OpenJPH 0.32.0): up to 8 runs of MEL events.
    fn decode(&mut self) {
        const MEL_EXP: [i32; 13] = [0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 4, 5];
        if self.bits < 6 {
            // if there are less than 6 bits in tmp
            self.read(); // then read from the MEL bitstream
        }
        while self.bits >= 6 && self.num_runs < 8 {
            let eval = MEL_EXP[self.k as usize]; // number of bits associated with state
            let mut run;
            if self.tmp & (1u64 << 63) != 0 {
                // The next bit to decode (stored in MSB): one is found
                run = 1 << eval;
                run -= 1; // consecutive runs of 0 events - 1
                self.k = if self.k + 1 < 12 { self.k + 1 } else { 12 }; // increment, max is 12
                self.tmp <<= 1; // consume one bit from tmp
                self.bits -= 1;
                run <<= 1; // a stretch of zeros not terminating in one
            } else {
                // 0 is found
                run = ((self.tmp >> (63 - eval)) as i32) & ((1 << eval) - 1);
                self.k = if self.k - 1 > 0 { self.k - 1 } else { 0 }; // decrement, min is 0
                self.tmp <<= eval + 1; // consume eval + 1 bits (max is 6)
                self.bits -= eval + 1;
                run = (run << 1) + 1; // a stretch of zeros terminating with one
            }
            let e = self.num_runs * 7; // 7 bits per run
            self.runs &= !(0x3Fu64 << e); // 6 bits are sufficient
            self.runs |= (run as u64) << e; // store the value in runs
            self.num_runs += 1; // increment count
        }
    }

    /// Port of `mel_init` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:221-251
    /// @ OpenJPH 0.32.0).
    pub(crate) fn init(bbuf: &'a [u8], lcup: i32, scup: i32) -> Self {
        let mut m = DecMel {
            buf: bbuf,
            data: (lcup - scup) as isize, // move the pointer to the start of MEL
            bits: 0,
            tmp: 0,
            unstuff: false,
            size: scup - 1, // size is the length of MEL+VLC-1
            k: 0,
            num_runs: 0,
            runs: 0,
        };
        // read bytes until data is 4-byte aligned
        let num = 4 - align4(bbuf, m.data) as i32;
        for _ in 0..num {
            // this code is similar to mel_read
            let mut d: u64 = if m.size > 0 {
                u64::from(bbuf[m.data as usize])
            } else {
                0xFF
            }; // if buffer is consumed
            if m.size == 1 {
                d |= 0xF; // if this is MEL+VLC-1, set LSBs to 0xF
            }
            // see the standard
            let advance = m.size > 0;
            m.size -= 1;
            m.data += isize::from(advance); // increment if the end is not reached
            let d_bits = 8 - i32::from(m.unstuff); // if unstuffing is needed, reduce by 1
            m.tmp = (m.tmp << d_bits) | d; // store bits in tmp
            m.bits += d_bits; // increment tmp by number of bits
            m.unstuff = (d & 0xFF) == 0xFF; // true of next byte needs unstuffing
        }
        m.tmp <<= 64 - m.bits; // push all the way up so the first bit is the MSB
        m
    }

    /// Port of `mel_get_run` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:260-269
    /// @ OpenJPH 0.32.0).
    pub(crate) fn get_run(&mut self) -> i32 {
        if self.num_runs == 0 {
            // if no runs, decode more bit from MEL segment
            self.decode();
        }
        let t = (self.runs & 0x7F) as i32; // retrieve one run
        self.runs >>= 7; // remove the retrieved run
        self.num_runs -= 1;
        t // return run
    }
}

/// Port of `rev_struct` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:275-284
/// @ OpenJPH 0.32.0): a stream read backwards, the VLC or the MagRef segment.
pub(crate) struct Rev<'a> {
    pub(crate) buf: &'a [u8],
    pub(crate) data: isize,
    pub(crate) tmp: u64,
    pub(crate) bits: u32,
    pub(crate) size: i32,
    pub(crate) unstuff: bool,
}

impl<'a> Rev<'a> {
    /// The 4 bytes before (and at) the read pointer, as `val`, the first byte read in its MSB.
    #[inline]
    fn fetch_val(&mut self) -> u32 {
        let mut val = 0u32;
        if self.size > 3 {
            // if there are more than 3 bytes left in VLC
            val = load_le_ui32(self.buf, self.data - 3); // then read 32 bits
            self.data -= 4; // move data pointer back by 4
            self.size -= 4; // reduce available byte by 4
        } else if self.size > 0 {
            // 4 or less
            let mut i = 24;
            while self.size > 0 {
                let v = u32::from(self.buf[self.data as usize]); // read one byte at a time
                self.data -= 1;
                val |= v << i; // put byte in its correct location
                self.size -= 1;
                i -= 8;
            }
        }
        val
    }

    /// The bits of `val` unstuffed into `tmp`.
    #[inline]
    fn add_val(&mut self, val: u32) {
        // accumulate in tmp, number of bits in tmp are stored in bits
        let mut tmp = val >> 24; // start with the MSB byte
        // test unstuff (previous byte is >0x8F), and this byte is 0x7F
        let mut bits = 8 - u32::from(self.unstuff && ((val >> 24) & 0x7F) == 0x7F);
        let mut unstuff = (val >> 24) > 0x8F; // this is for the next byte

        tmp |= ((val >> 16) & 0xFF) << bits; // process the next byte
        bits += 8 - u32::from(unstuff && ((val >> 16) & 0x7F) == 0x7F);
        unstuff = ((val >> 16) & 0xFF) > 0x8F;

        tmp |= ((val >> 8) & 0xFF) << bits;
        bits += 8 - u32::from(unstuff && ((val >> 8) & 0x7F) == 0x7F);
        unstuff = ((val >> 8) & 0xFF) > 0x8F;

        tmp |= (val & 0xFF) << bits;
        bits += 8 - u32::from(unstuff && (val & 0x7F) == 0x7F);
        unstuff = (val & 0xFF) > 0x8F;

        // now move the read and unstuffed bits into vlcp->tmp
        self.tmp |= u64::from(tmp) << self.bits;
        self.bits += bits;
        self.unstuff = unstuff; // this for the next read
    }

    /// Port of `rev_read` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:308-357
    /// @ OpenJPH 0.32.0), and `rev_read_mrp` (lines 453-499), which is the same.
    fn read(&mut self) {
        // process 4 bytes at a time
        if self.bits > 32 {
            // if there are more than 32 bits in tmp, then reading 32 bits can overflow tmp
            return;
        }
        let val = self.fetch_val();
        self.add_val(val);
    }

    /// Port of `rev_init` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:374-405
    /// @ OpenJPH 0.32.0): the VLC segment, from its end.
    fn init_vlc(data: &'a [u8], lcup: i32, scup: i32) -> Self {
        // first byte has only the upper 4 bits
        let mut v = Rev {
            buf: data,
            data: (lcup - 2) as isize,
            size: scup - 2,
            tmp: 0,
            bits: 0,
            unstuff: false,
        };
        let d = u32::from(data[v.data as usize]); // read one byte (this is a half byte)
        v.data -= 1;
        v.tmp = u64::from(d >> 4); // both initialize and set
        v.bits = 4 - u32::from((v.tmp & 7) == 7); // check standard
        #[allow(clippy::bad_bit_mask, clippy::ineffective_bit_mask)] // as upstream writes it
        {
            v.unstuff = (d | 0xF) > 0x8F; // this is useful for the next byte
        }

        // This code is designed for an architecture that read address should align to the
        // read size (address multiple of 4 if read size is 4)
        let num = 1 + align4(data, v.data) as i32;
        let tnum = if num < v.size { num } else { v.size };
        for _ in 0..tnum {
            let d = u64::from(data[v.data as usize]); // read one byte and move read pointer
            v.data -= 1;
            // check if the last byte was >0x8F (unstuff == true) and this is 0x7F
            let d_bits = 8 - u32::from(v.unstuff && (d & 0x7F) == 0x7F);
            v.tmp |= d << v.bits; // move data to vlcp->tmp
            v.bits += d_bits;
            v.unstuff = d > 0x8F; // for next byte
        }
        v.size -= tnum;
        v.read(); // read another 32 buts
        v
    }

    /// Port of `rev_fetch` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:415-424
    /// @ OpenJPH 0.32.0), and `rev_fetch_mrp` (lines 551-560).
    #[inline]
    pub(crate) fn fetch(&mut self) -> u32 {
        if self.bits < 32 {
            // if there are less then 32 bits, read more
            self.read(); // read 32 bits, but unstuffing might reduce this
            if self.bits < 32 {
                // if there is still space in vlcp->tmp for 32 bits
                self.read(); // read another 32
            }
        }
        self.tmp as u32 // return the head (bottom-most) of vlcp->tmp
    }

    /// Port of `rev_advance` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:433-439
    /// @ OpenJPH 0.32.0), and `rev_advance_mrp` (lines 569-575).
    #[inline]
    pub(crate) fn advance(&mut self, num_bits: u32) -> u32 {
        self.tmp >>= num_bits; // remove bits
        self.bits -= num_bits; // decrement the number of bits
        self.tmp as u32
    }

    /// Port of `rev_init_mrp` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:517-541
    /// @ OpenJPH 0.32.0): the MagRef segment, from its end.
    pub(crate) fn init_mrp(data: &'a [u8], lcup: i32, len2: i32) -> Self {
        let mut m = Rev {
            buf: data,
            data: (lcup + len2 - 1) as isize,
            size: len2,
            unstuff: true,
            bits: 0,
            tmp: 0,
        };
        // This code is designed for an architecture that read address should align to the
        // read size (address multiple of 4 if read size is 4)
        let num = 1 + align4(data, m.data) as i32;
        for _ in 0..num {
            let has = m.size > 0;
            m.size -= 1;
            // read a byte, 0 if no more data
            let d: u64 = if has {
                let b = u64::from(data[m.data as usize]);
                m.data -= 1;
                b
            } else {
                0
            };
            // check if unstuffing is needed
            let d_bits = 8 - u32::from(m.unstuff && (d & 0x7F) == 0x7F);
            m.tmp |= d << m.bits; // move data to vlcp->tmp
            m.bits += d_bits;
            m.unstuff = d > 0x8F; // for next byte
        }
        m.read();
        m
    }
}

/// Port of `frwd_struct32` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:582-588
/// @ OpenJPH 0.32.0): a stream read forwards, the MagSgn (fed with `X` = 0xFF past its end) or
/// the SigProp (0) segment.
pub(crate) struct Frwd<'a, const X: u32> {
    pub(crate) buf: &'a [u8],
    pub(crate) data: isize,
    pub(crate) tmp: u64,
    pub(crate) bits: u32,
    pub(crate) unstuff: u32,
    pub(crate) size: i32,
}

impl<'a, const X: u32> Frwd<'a, X> {
    /// Port of `frwd_read` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:609-653
    /// @ OpenJPH 0.32.0).
    fn read(&mut self) {
        let mut val: u32;
        if self.size > 3 {
            val = load_le_ui32(self.buf, self.data); // read 32 bits
            self.data += 4; // increment pointer
            self.size -= 4; // reduce size
        } else if self.size > 0 {
            let mut i = 0;
            val = if X != 0 { 0xFFFFFFFF } else { 0 };
            while self.size > 0 {
                let v = u32::from(self.buf[self.data as usize]); // read one byte at a time
                self.data += 1;
                let m = !(0xFFu32 << i); // mask of location
                val = (val & m) | (v << i); // put one byte in its correct location
                self.size -= 1;
                i += 8;
            }
        } else {
            val = if X != 0 { 0xFFFFFFFF } else { 0 };
        }

        // we accumulate in t and keep a count of the number of bits in bits
        let mut bits = 8 - self.unstuff;
        let mut t = val & 0xFF;
        let mut unstuff = (val & 0xFF) == 0xFF; // Do we need unstuffing next?

        t |= ((val >> 8) & 0xFF) << bits;
        bits += 8 - u32::from(unstuff);
        unstuff = ((val >> 8) & 0xFF) == 0xFF;

        t |= ((val >> 16) & 0xFF) << bits;
        bits += 8 - u32::from(unstuff);
        unstuff = ((val >> 16) & 0xFF) == 0xFF;

        t |= ((val >> 24) & 0xFF) << bits;
        bits += 8 - u32::from(unstuff);
        self.unstuff = u32::from(((val >> 24) & 0xFF) == 0xFF); // for next byte

        self.tmp |= u64::from(t) << self.bits; // move data to msp->tmp
        self.bits += bits;
    }

    /// Port of `frwd_init` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:666-689
    /// @ OpenJPH 0.32.0).
    pub(crate) fn init(data: &'a [u8], start: isize, size: i32) -> Self {
        let mut m = Frwd {
            buf: data,
            data: start,
            tmp: 0,
            bits: 0,
            unstuff: 0,
            size,
        };
        // This code is designed for an architecture that read address should align to the
        // read size (address multiple of 4 if read size is 4)
        let num = 4 - align4(data, m.data) as i32;
        for _ in 0..num {
            let has = m.size > 0;
            m.size -= 1;
            let d: u64 = if has {
                let b = u64::from(data[m.data as usize]);
                m.data += 1;
                b
            } else {
                u64::from(X)
            };
            m.tmp |= d << m.bits; // store data in msp->tmp
            m.bits += 8 - m.unstuff; // number of bits added to msp->tmp
            m.unstuff = u32::from((d & 0xFF) == 0xFF); // unstuffing for next byte
        }
        m.read(); // read 32 bits more
        m
    }

    /// Port of `frwd_advance` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:698-703
    /// @ OpenJPH 0.32.0).
    #[inline]
    pub(crate) fn advance(&mut self, num_bits: u32) {
        self.tmp >>= num_bits; // consume num_bits
        self.bits -= num_bits;
    }

    /// Port of `frwd_fetch` (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:714-723
    /// @ OpenJPH 0.32.0).
    #[inline]
    pub(crate) fn fetch(&mut self) -> u32 {
        if self.bits < 32 {
            self.read();
            if self.bits < 32 {
                // need to test
                self.read();
            }
        }
        self.tmp as u32
    }
}

/// `(1 << m_n) - 1` as C's `int` arithmetic gives its bits.
#[inline]
fn low_mask(m_n: u32) -> u32 {
    1u32.wrapping_shl(m_n).wrapping_sub(1)
}

/// One sample of a quad from the MagSgn stream: the sample's value, and its `v_n`.
#[inline]
fn decode_sample(magsgn: &mut Frwd<'_, 0xFF>, inf: u32, u_q: u32, bit: u32, p: u32) -> (u32, u32) {
    let mut v_n = 0;
    let mut val = 0;
    if inf & (1 << (4 + bit)) != 0 {
        // get 32 bits of magsgn data
        let ms_val = magsgn.fetch();
        let m_n = u_q.wrapping_sub((inf >> (12 + bit)) & 1); // remove e_k
        magsgn.advance(m_n); // consume m_n

        val = ms_val << 31; // get sign bit
        v_n = ms_val & low_mask(m_n); // keep only m_n bits
        v_n |= ((inf >> (8 + bit)) & 1) << m_n; // add EMB e_1 as MSB
        v_n |= 1; // add center of bin
        // v_n now has 2 * (\mu - 1) + 0.5 with correct sign bit; add 2 to make it
        // 2*\mu+0.5, shift it up to missing MSBs
        val |= (v_n + 2) << (p - 1);
    }
    (val, v_n)
}

static INSUFFICIENT_PRECISION: AtomicBool = AtomicBool::new(false);
static MODIFY_CODE: AtomicBool = AtomicBool::new(false);
static TRUNCATE_SPP_MRP: AtomicBool = AtomicBool::new(false);

/// Port of `ojph_decode_codeblock32`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:742-1614 @ OpenJPH 0.32.0): a codeblock of `width` x `height` samples decoded into
/// `decoded_data` (rows `stride` apart), as sign and magnitude, the magnitude's bin center
/// set; false when the codeblock cannot be decoded. Upstream's warnings that show once per
/// process show once per process here too.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ojph_decode_codeblock32(
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
) -> bool {
    let t = tables();
    let stride = stride as usize;

    if num_passes > 1 && lengths2 == 0 {
        warn(
            0x00010001,
            FILE,
            754,
            "A malformed codeblock that has more than one coding pass, but zero length for 2nd and potential 3rd pass.",
            &[],
        );
        num_passes = 1;
    }
    if num_passes > 3 {
        warn(
            0x00010002,
            FILE,
            762,
            "We do not support more than 3 coding passes; This codeblocks has %d passes.",
            &[Arg::Int(num_passes.into())],
        );
        return false;
    }

    if missing_msbs > 30 {
        // p < 0
        if !INSUFFICIENT_PRECISION.swap(true, Ordering::Relaxed) {
            warn(
                0x00010003,
                FILE,
                773,
                "32 bits are not enough to decode this codeblock. This message will not be displayed again.",
                &[],
            );
        }
        return false;
    } else if missing_msbs == 30 {
        // p == 0: not enough precision to decode and set the bin center to 1
        if !MODIFY_CODE.swap(true, Ordering::Relaxed) {
            warn(
                0x00010004,
                FILE,
                783,
                "Not enough precision to decode the cleanup pass. The code can be modified to support this case. This message will not be displayed again.",
                &[],
            );
        }
        return false; // 32 bits are not enough to decode this
    } else if missing_msbs == 29 {
        // if p is 1, then num_passes must be 1
        if num_passes > 1 {
            num_passes = 1;
            if !TRUNCATE_SPP_MRP.swap(true, Ordering::Relaxed) {
                warn(
                    0x00010005,
                    FILE,
                    796,
                    "Not enough precision to decode the SgnProp nor MagRef passes; both will be skipped. This message will not be displayed again.",
                    &[],
                );
            }
        }
    }
    let p = 30 - missing_msbs; // The least significant bitplane for CUP

    if lengths1 < 2 {
        warn(0x00010006, FILE, 809, "Wrong codeblock length.", &[]);
        return false;
    }

    // read scup and fix the bytes there
    let lcup = lengths1 as i32; // length of CUP
    // scup is the length of MEL + VLC
    let scup = (i32::from(coded_data[(lcup - 1) as usize]) << 4)
        + i32::from(coded_data[(lcup - 2) as usize] & 0xF);
    if scup < 2 || scup > lcup || scup > 4079 {
        // something is wrong
        return false;
    }

    // The temporary storage scratch holds two types of data in an interleaved fashion. The
    // interleaving allows us to use one memory pointer. We have one entry for a decoded VLC
    // code, and one entry for UVLC.
    let mut scratch = [0u16; 8 * 513]; // 8 kB

    // We need an extra two entries (one inf and one u_q) beyond the last column. If the
    // block width is 4 (2 quads), then we use sstr of 8 (enough for 4 quads). If width is 8
    // (4 quads) we use sstr is 16 (enough for 8 quads). For a width of 16 (8 quads), we use 24
    // (enough for 12 quads).
    let sstr = (((width + 2) + 7) & !7) as usize; // multiples of 8

    let mmsbp2 = missing_msbs + 2;

    // The cleanup pass is decoded in two steps; in step one, the VLC and MEL segments are
    // decoded, generating a record that has 2 bytes per quad. The 2 bytes contain, u, rho,
    // e^1 & e^k. This information should be sufficient for the next step. In step 2, we
    // decode the MagSgn segment.

    // step 1 decoding VLC and MEL segments
    {
        // init structures
        let mut mel = DecMel::init(coded_data, lcup, scup);
        let mut vlc = Rev::init_vlc(coded_data, lcup, scup);

        let mut run = mel.get_run(); // decode runs of events from MEL bitstrm
        // data represented as runs of 0 events; See mel_decode description

        let mut vlc_val;
        let mut c_q: u32 = 0;
        let mut sp = 0usize;
        // initial quad row
        let mut x = 0u32;
        while x < width {
            // decode VLC

            // first quad
            vlc_val = vlc.fetch();

            // decode VLC using the context c_q and the head of the VLC bitstream
            let mut t0 = t.vlc_tbl0[(c_q + (vlc_val & 0x7F)) as usize];

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
                run -= 2; // subtract 2, since events number if multiplied by 2

                uvlc_mode += if run == -1 { 0x40 } else { 0 }; // increment uvlc_mode by is
                // 0x40

                if run < 0 {
                    // if run is consumed (run is -1 or -2), get another run
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
            let tmp = vlc_val & low_mask(len); // suffix value for 2 quads
            vlc.advance(len);
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
        let mut y = 2u32;
        while y < height {
            c_q = 0; // context
            let mut sp = (y as usize >> 1) * sstr; // this row of quads
            let mut x = 0u32;
            while x < width {
                // decode VLC

                // first quad
                c_q |= (u32::from(scratch[sp - sstr]) & 0xA0) << 2;
                c_q |= (u32::from(scratch[sp + 2 - sstr]) & 0x20) << 4;

                // get context
                vlc_val = vlc.fetch();

                // decode VLC using the context c_q and the head of the VLC bitstream
                let mut t0 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F)) as usize];

                // if context is zero, use one MEL event
                if c_q == 0 {
                    // zero context
                    run -= 2; // subtract 2, since events number is multiplied by 2

                    // Is the run terminated in 1? if so, use decoded VLC code, otherwise,
                    // discard decoded data, since we will decoded again using a different
                    // context
                    t0 = if run == -1 { t0 } else { 0 };

                    // is run -1 or -2? this means a run has been consumed
                    if run < 0 {
                        run = mel.get_run(); // get another run
                    }
                }
                scratch[sp] = t0;
                x += 2;

                // prepare context for the next quad; eqn. 2 in ITU T.814 sigma_q (n, ne, nf)
                c_q = ((u32::from(t0) & 0x40) << 2) | ((u32::from(t0) & 0x80) << 1);
                // sigma_q (w, sw)
                c_q |= u32::from(scratch[sp - sstr]) & 0x80;
                // sigma_q (nw)
                c_q |= (u32::from(scratch[sp + 2 - sstr]) & 0xA0) << 2;
                // sigma_q (ne, nf)
                c_q |= (u32::from(scratch[sp + 4 - sstr]) & 0x20) << 4;

                // remove data from vlc stream (0 bits are removed if vlc is unused)
                vlc_val = vlc.advance(u32::from(t0) & 0x7);

                // second quad
                let mut t1 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F)) as usize];

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
                scratch[sp + 2] = t1;
                x += 2;

                // partial c_q, will be completed when we process the next quad
                c_q = ((u32::from(t1) & 0x40) << 2) | ((u32::from(t1) & 0x80) << 1);
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
                let tmp = vlc_val & low_mask(len); // suffix value for 2 quads
                vlc.advance(len);
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

    // step 2: we decode magsgn
    {
        // We allocate a scratch row for storing v_n values. We have 512 quads horizontally.
        // We need an extra entry to handle the case of vp[1] when vp is at the last column.
        // Here, we allocate 4 instead of 1 to make the buffer size a multipled of 16 bytes.
        const V_N_SIZE: usize = 512 + 4;
        let mut v_n_scratch = [0u32; V_N_SIZE]; // 2+ kB

        let mut magsgn = Frwd::<0xFF>::init(coded_data, 0, lcup - scup);

        let mut sp = 0usize;
        let mut vp = 0usize;
        let mut dp = 0usize;

        let mut prev_v_n = 0u32;
        let mut x = 0u32;
        while x < width {
            let inf = u32::from(scratch[sp]);
            let u_q = u32::from(scratch[sp + 1]);
            if u_q > mmsbp2 {
                return false;
            }

            // (the first sample's v_n is not kept, as upstream's is overwritten)
            let (val, _) = decode_sample(&mut magsgn, inf, u_q, 0, p);
            decoded_data[dp] = val;
            let (val, v_n) = decode_sample(&mut magsgn, inf, u_q, 1, p);
            decoded_data[dp + stride] = val;
            v_n_scratch[vp] = prev_v_n | v_n;
            prev_v_n = 0;
            dp += 1;
            x += 1;
            if x >= width {
                // if this is true, then width is odd
                vp += 1;
                break;
            }

            let (val, _) = decode_sample(&mut magsgn, inf, u_q, 2, p);
            decoded_data[dp] = val;
            let (val, v_n) = decode_sample(&mut magsgn, inf, u_q, 3, p);
            decoded_data[dp + stride] = val;
            prev_v_n = v_n;
            dp += 1;
            x += 1;
            sp += 2;
            vp += 1;
        }
        v_n_scratch[vp] = prev_v_n;

        let mut y = 2u32;
        while y < height {
            let mut sp = (y as usize >> 1) * sstr;
            let mut vp = 0usize;
            let mut dp = y as usize * stride;
            prev_v_n = 0;
            let mut x = 0u32;
            while x < width {
                let inf = u32::from(scratch[sp]);
                let u_q = u32::from(scratch[sp + 1]);

                let mut gamma = inf & 0xF0;
                gamma &= gamma.wrapping_sub(0x10); // is gamma_q 1?
                let mut emax = v_n_scratch[vp] | v_n_scratch[vp + 1];
                emax = 31 - count_leading_zeros(emax | 2); // emax - 1
                let kappa = if gamma != 0 { emax } else { 1 };

                let u_q = u_q + kappa;
                if u_q > mmsbp2 {
                    return false;
                }

                // (the first sample's v_n is not kept, as upstream's is overwritten)
                let (val, _) = decode_sample(&mut magsgn, inf, u_q, 0, p);
                decoded_data[dp] = val;
                let (val, v_n) = decode_sample(&mut magsgn, inf, u_q, 1, p);
                decoded_data[dp + stride] = val;
                v_n_scratch[vp] = prev_v_n | v_n;
                prev_v_n = 0;
                dp += 1;
                x += 1;
                if x >= width {
                    // if this is true, then width is odd
                    vp += 1;
                    break;
                }

                let (val, _) = decode_sample(&mut magsgn, inf, u_q, 2, p);
                decoded_data[dp] = val;
                let (val, v_n) = decode_sample(&mut magsgn, inf, u_q, 3, p);
                decoded_data[dp + stride] = val;
                prev_v_n = v_n;
                dp += 1;
                x += 1;
                sp += 2;
                vp += 1;
            }
            v_n_scratch[vp] = prev_v_n;
            y += 2;
        }
    }

    if num_passes > 1 {
        // We use scratch again, we can divide it into multiple regions. sigma holds all the
        // significant samples, and it cannot be modified after it is set. it will be used
        // during the Magnitude Refinement Pass
        let sigma = 0usize;

        let mut mstr = (width + 3) >> 2; // divide by 4, since each ui16 contains 4 columns
        mstr = ((mstr + 2) + 7) & !7; // multiples of 8
        let mstr = mstr as usize;

        // We re-arrange quad significance, where each 4 consecutive bits represent one quad,
        // into column significance, where, each 4 consequtive bits represent one column of 4
        // rows
        {
            let mut y = 0u32;
            while y < height {
                let mut sp = (y as usize >> 1) * sstr;
                let mut dp = sigma + (y as usize >> 2) * mstr;
                let mut x = 0u32;
                while x < width {
                    let s0 = u32::from(scratch[sp]);
                    let s2 = u32::from(scratch[sp + 2]);
                    let s0n = u32::from(scratch[sp + sstr]);
                    let s2n = u32::from(scratch[sp + 2 + sstr]);
                    let mut t0 = ((s0 & 0x30) >> 4) | ((s0 & 0xC0) >> 2);
                    t0 |= ((s2 & 0x30) << 4) | ((s2 & 0xC0) << 6);
                    let mut t1 = ((s0n & 0x30) >> 2) | (s0n & 0xC0);
                    t1 |= ((s2n & 0x30) << 6) | ((s2n & 0xC0) << 8);
                    scratch[dp] = (t0 | t1) as u16;
                    x += 4;
                    sp += 4;
                    dp += 1;
                }
                scratch[dp] = 0; // set an extra entry on the right with 0
                y += 4;
            }
            // reset one row after the codeblock
            {
                let mut dp = sigma + (y as usize >> 2) * mstr;
                let mut x = 0u32;
                while x < width {
                    scratch[dp] = 0;
                    x += 4;
                    dp += 1;
                }
                scratch[dp] = 0; // set an extra entry on the right with 0
            }
        }

        // We perform Significance Propagation Pass here
        {
            // This stores significance information of the previous 4 rows. Significance
            // information in this array includes all signicant samples in bitplane p - 1;
            // that is, significant samples for bitplane p (discovered during the cleanup
            // pass and stored in sigma) and samples that have recently became significant
            // (during the SPP) in bitplane p-1. We store enough for the widest row,
            // containing 1024 columns, which is equivalent to 256 of ui16, since each stores
            // 4 columns. We add an extra 8 entries, just in case we need more
            let mut prev_row_sig = [0u16; 256 + 8]; // 528 Bytes

            let mut sigprop = Frwd::<0>::init(coded_data, lengths1 as isize, lengths2 as i32);

            let mut y = 0u32;
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
                let mut prev = 0u32;
                let mut prev_sig = 0usize;
                let mut cur_sig = sigma + (y as usize >> 2) * mstr;
                let dpp = y as usize * stride;
                let mut x = 0u32;
                while x < width {
                    // only rows and columns inside the stripe are included
                    let mut s = x as i32 + 4 - width as i32;
                    s = if s > 0 { s } else { 0 };
                    pattern >>= s * 4;

                    // We first find locations that need to be tested (potential SPP members);
                    // these location will end up in mbr. In each iteration, we produce 16
                    // bits because cwd can have up to 16 bits of significance information,
                    // followed by the corresponding 16 bits of sign information; therefore,
                    // it is sufficient to fetch 32 bit data per loop.

                    // Althougth we are interested in 16 bits only, we load 32 bits. For the
                    // 16 bits we are producing, we need the next 4 bits -- we need data for
                    // at least 5 columns out of 8. Therefore loading 32 bits is easier than
                    // loading 16 bits twice.
                    let ps = u32::from(prev_row_sig[prev_sig])
                        | (u32::from(prev_row_sig[prev_sig + 1]) << 16);
                    let ns = u32::from(scratch[cur_sig + mstr])
                        | (u32::from(scratch[cur_sig + mstr + 1]) << 16);
                    let mut u = (ps & 0x88888888) >> 3; // the row on top
                    if !stripe_causal {
                        u |= (ns & 0x11111111) << 3; // the row below
                    }

                    let cs = u32::from(scratch[cur_sig]) | (u32::from(scratch[cur_sig + 1]) << 16);
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
                        let mut cwd = sigprop.fetch();

                        let mut cnt = 0u32;
                        let mut col_mask = 0xFu32;
                        let inv_sig = !cs & pattern;
                        let mut i = 0;
                        while i < 16 {
                            if (col_mask & new_sig) == 0 {
                                i += 4;
                                col_mask <<= 4;
                                continue;
                            }

                            // scan one column
                            let mut sample_mask = 0x1111u32 & col_mask;
                            if new_sig & sample_mask != 0 {
                                new_sig &= !sample_mask;
                                if cwd & 1 != 0 {
                                    let t = 0x33u32 << i;
                                    new_sig |= t & inv_sig;
                                }
                                cwd >>= 1;
                                cnt += 1;
                            }

                            sample_mask <<= 1;
                            if new_sig & sample_mask != 0 {
                                new_sig &= !sample_mask;
                                if cwd & 1 != 0 {
                                    let t = 0x76u32 << i;
                                    new_sig |= t & inv_sig;
                                }
                                cwd >>= 1;
                                cnt += 1;
                            }

                            sample_mask <<= 1;
                            if new_sig & sample_mask != 0 {
                                new_sig &= !sample_mask;
                                if cwd & 1 != 0 {
                                    let t = 0xECu32 << i;
                                    new_sig |= t & inv_sig;
                                }
                                cwd >>= 1;
                                cnt += 1;
                            }

                            sample_mask <<= 1;
                            if new_sig & sample_mask != 0 {
                                new_sig &= !sample_mask;
                                if cwd & 1 != 0 {
                                    let t = 0xC8u32 << i;
                                    new_sig |= t & inv_sig;
                                }
                                cwd >>= 1;
                                cnt += 1;
                            }
                            i += 4;
                            col_mask <<= 4;
                        }

                        // obtain signs here
                        if new_sig != 0 {
                            // if new_sig has bits
                            let val = 3u32 << (p - 2); // sign and bin center
                            col_mask = 0xF;
                            for dp in (dpp + x as usize..).take(4) {
                                if (col_mask & new_sig) != 0 {
                                    // scan 4 signs
                                    let mut sample_mask = 0x1111u32 & col_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp] = (cwd << 31) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + stride] = (cwd << 31) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + 2 * stride] = (cwd << 31) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + 3 * stride] = (cwd << 31) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }
                                }
                                col_mask <<= 4;
                            }
                        }
                        sigprop.advance(cnt);
                    }

                    new_sig |= cs;
                    prev_row_sig[prev_sig] = new_sig as u16;

                    // vertical integration for the new sig. info.
                    let t = new_sig;
                    new_sig |= (t & 0x7777) << 1; // above neighbors
                    new_sig |= (t & 0xEEEE) >> 1; // below neighbors
                    // add sig. info. from the row on top and below
                    prev = new_sig | u;
                    // we need only the bits in 0xF000
                    prev &= 0xF000;

                    x += 4;
                    cur_sig += 1;
                    prev_sig += 1;
                }
                y += 4;
            }
        }

        // We perform Magnitude Refinement Pass here
        if num_passes > 2 {
            let mut magref = Rev::init_mrp(coded_data, lengths1 as i32, lengths2 as i32);

            let mut y = 0u32;
            while y < height {
                let mut cur_sig = sigma + (y as usize >> 2) * mstr;
                let dpp = y as usize * stride;
                let half = 1u32 << (p - 2);
                let mut i = 0u32;
                while i < width {
                    // Process one entry from sigma array at a time; Each nibble (4 bits) in
                    // the sigma array represents 4 rows, and the 32 bits contain 8 columns
                    let mut cwd = magref.fetch(); // get 32 bit data
                    let sig = u32::from(scratch[cur_sig]) | (u32::from(scratch[cur_sig + 1]) << 16); // 32 bits processed now
                    let mut col_mask = 0xFu32; // a mask for a column in sig
                    if sig != 0 {
                        // if any of the 32 bits are set
                        for j in 0..8usize {
                            // one column at a time
                            if sig & col_mask != 0 {
                                // lowest nibble
                                let mut dp = dpp + i as usize + j; // next column in decoded samples
                                let mut sample_mask = 0x11111111u32 & col_mask; // LSB

                                for _ in 0..4 {
                                    if sig & sample_mask != 0 {
                                        // if LSB is set
                                        let mut sym = cwd & 1; // get it value
                                        // remove center of bin if sym is 0
                                        sym = (1 - sym) << (p - 1); // previous center of bin
                                        sym |= half; // put half the center of bin
                                        decoded_data[dp] ^= sym; // remove old bin center and put new
                                        cwd >>= 1; // consume word
                                    }
                                    sample_mask += sample_mask; // next row
                                    dp += stride; // next samples row
                                }
                            }
                            col_mask <<= 4; // next column
                        }
                    }
                    // consume data according to the number of bits set
                    magref.advance(population_count(sig));
                    i += 8;
                    cur_sig += 2;
                }
                y += 4;
            }
        }
    }
    true
}
