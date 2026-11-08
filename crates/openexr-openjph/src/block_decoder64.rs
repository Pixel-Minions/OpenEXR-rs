// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2022, Aous Naman

//! Port of `ojph_block_decoder64.cpp`
//! (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:1-1662 @ OpenJPH 0.32.0): the
//! HT block decoder for samples of up to 63 magnitude bits. Its MEL decoder and MagRef reader
//! are the 32-bit decoder's (`block_decoder32`); its VLC and MagSgn readers take a byte at a
//! time, and its u values may carry an extension.

use crate::arch::{count_leading_zeros64, population_count};
use crate::block_common::tables;
use crate::block_decoder32::{DecMel, Frwd, Rev};
use crate::message::{Arg, warn};

const FILE: &str = "ojph_block_decoder64.cpp";

impl<'a> Rev<'a> {
    /// Port of `rev_read8` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:304-321
    /// @ OpenJPH 0.32.0): one more byte, 0 past the segment's start.
    fn read8(&mut self) {
        let mut val: u8 = 0; // insert 0s at the end -- the standard says that the
        // VLC segment can be a fraction of a byte
        if self.size > 0 {
            val = self.buf[self.data as usize]; // then read 8 bits
            self.data -= 1; // increment data pointer
            self.size -= 1; // decrement number of bytes in the buffer
        }
        // accumulate in tmp, and keep count in bits
        let t = u8::from(self.unstuff && (val & 0x7F) == 0x7F);
        val &= 0xFFu8 >> t; // protect against erroneous 1 in MSB
        self.tmp |= u64::from(val) << self.bits;
        self.bits += 8 - u32::from(t);
        self.unstuff = val > 0x8F;
    }

    /// Port of `rev_init8` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:342-356
    /// @ OpenJPH 0.32.0): the VLC segment, from its end.
    fn init_vlc8(data: &'a [u8], lcup: i32, scup: i32) -> Self {
        let mut v = Rev {
            buf: data,
            data: (lcup - 2) as isize,
            size: scup - 2,
            tmp: 0,
            bits: 0,
            unstuff: false,
        };
        let mut val = data[v.data as usize]; // read one byte (this is a half byte)
        v.data -= 1;
        val >>= 4;
        let t = u8::from((val & 0x7) == 0x7); // unstuffing is needed
        val &= 0xFu8 >> t; // protect against erroneous 1 in MSB
        v.tmp = u64::from(val);
        v.bits = 4 - u32::from(t);
        v.unstuff = val > 0x8; // this is useful for the next byte
        v
    }

    /// Port of `rev_fetch64` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:370-376
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn fetch64(&mut self) -> u64 {
        while self.bits <= 56 {
            self.read8(); // read 8 bits, but unstuffing might reduce this
        }
        self.tmp // return unstuff decoded bits
    }

    /// Port of `rev_advance64`
    /// (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:384-391 @ OpenJPH 0.32.0).
    #[inline]
    fn advance64(&mut self, num_bits: u32) -> u64 {
        self.tmp >>= num_bits; // remove bits
        self.bits -= num_bits; // decrement the number of bits
        self.tmp
    }
}

impl<'a> Frwd<'a, 0xFF> {
    /// Port of `frwd_read8` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:624-638
    /// @ OpenJPH 0.32.0): one more byte, 0xFF past the segment's end.
    fn read8(&mut self) {
        let mut val: u8 = 0xFF;
        if self.size > 0 {
            val = self.buf[self.data as usize]; // read 8 bits
            self.data += 1; // increment pointer
            self.size -= 1; // reduce size
        }
        // unstuff and accumulate
        let t = u8::from(self.unstuff != 0);
        val &= 0xFFu8 >> t;
        self.unstuff = u32::from(val == 0xFF);
        self.tmp |= u64::from(val) << self.bits; // move data to msp->tmp
        self.bits += 8 - u32::from(t);
    }

    /// Port of `frwd_init8` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:689-698
    /// @ OpenJPH 0.32.0).
    fn init8(data: &'a [u8], size: i32) -> Self {
        let mut m = Frwd {
            buf: data,
            data: 0,
            tmp: 0,
            bits: 0,
            unstuff: 0,
            size,
        };
        m.read8(); // read 8 bits
        m
    }

    /// Port of `frwd_fetch64` (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:741-747
    /// @ OpenJPH 0.32.0).
    #[inline]
    fn fetch64(&mut self) -> u64 {
        while self.bits <= 56 {
            self.read8();
        }
        self.tmp
    }
}

/// One sample of a quad from the MagSgn stream, in 64 bits: the sample's value, and its `v_n`.
#[inline]
fn decode_sample64(
    magsgn: &mut Frwd<'_, 0xFF>,
    inf: u32,
    u_q: u32,
    bit: u32,
    p: u32,
) -> (u64, u64) {
    let mut v_n = 0;
    let mut val = 0;
    if inf & (1 << (4 + bit)) != 0 {
        // get 64 bits of magsgn data
        let ms_val = magsgn.fetch64();
        let m_n = u_q.wrapping_sub((inf >> (12 + bit)) & 1); // remove e_k
        magsgn.advance(m_n); // consume m_n

        val = ms_val << 63; // get sign bit
        v_n = ms_val & 1u64.wrapping_shl(m_n).wrapping_sub(1); // keep only m_n bits
        v_n |= u64::from((inf >> (8 + bit)) & 1) << m_n; // add EMB e_1 as MSB
        v_n |= 1; // add center of bin
        // v_n now has 2 * (\mu - 1) + 0.5 with correct sign bit; add 2 to make it
        // 2*\mu+0.5, shift it up to missing MSBs
        val |= (v_n + 2) << (p - 1);
    }
    (val, v_n)
}

/// `(1 << m_n) - 1` as C's `int` arithmetic gives its bits.
#[inline]
fn low_mask(m_n: u32) -> u32 {
    1u32.wrapping_shl(m_n).wrapping_sub(1)
}

/// Port of `ojph_decode_codeblock64`
/// (external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:766-1661 @ OpenJPH 0.32.0): a codeblock of `width` x `height` samples decoded into
/// `decoded_data` (rows `stride` apart), as sign and magnitude, the magnitude's bin center
/// set; false when the codeblock cannot be decoded. Upstream's warnings that show once per
/// process show once per process here too.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ojph_decode_codeblock64(
    coded_data: &[u8],
    decoded_data: &mut [u64],
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
            778,
            "A malformed codeblock that has more than one coding pass, but zero length for 2nd and potential 3rd pass.",
            &[],
        );
        num_passes = 1;
    }
    if num_passes > 3 {
        warn(
            0x00010002,
            FILE,
            786,
            "We do not support more than 3 coding passes; This codeblocks has %d passes.",
            &[Arg::Int(num_passes.into())],
        );
        return false;
    }

    let p = 62 - missing_msbs; // The least significant bitplane for CUP

    if lengths1 < 2 {
        warn(0x00010006, FILE, 832, "Wrong codeblock length.", &[]);
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
        let mut vlc = Rev::init_vlc8(coded_data, lcup, scup);

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
            vlc_val = vlc.fetch64();

            // decode VLC using the context c_q and the head of the VLC bitstream
            let mut t0 = t.vlc_tbl0[(c_q + (vlc_val & 0x7F) as u32) as usize];

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
            vlc_val = vlc.advance64(u32::from(t0) & 0x7);

            // second quad
            let mut t1 = t.vlc_tbl0[(c_q + (vlc_val & 0x7F) as u32) as usize];

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
            vlc_val = vlc.advance64(u32::from(t1) & 0x7);

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
            let idx = uvlc_mode + (vlc_val & 0x3F) as u32;
            let mut uvlc_entry = u32::from(t.uvlc_tbl0[idx as usize]);
            let u_bias = u16::from(t.uvlc_bias[idx as usize]);
            // remove total prefix length
            vlc_val = vlc.advance64(uvlc_entry & 0x7);
            uvlc_entry >>= 3;
            // extract suffixes for quad 0 and 1
            let mut len = uvlc_entry & 0xF; // suffix length for 2 quads
            let tmp = (vlc_val & u64::from(low_mask(len))) as u32; // suffix value for 2 quads
            vlc_val = vlc.advance64(len);
            uvlc_entry >>= 4;
            // quad 0 length
            len = uvlc_entry & 0x7; // quad 0 suffix length
            uvlc_entry >>= 3;
            let mut u_q0 = ((uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16;
            let mut u_q1 = ((uvlc_entry >> 3) + (tmp >> len)) as u16;

            // decode u_q extensions, which is needed only when bitplane_count > 32
            let cond0 = i32::from(u_q0) - i32::from(u_bias & 0x3) > 32;
            let mut u_ext = if cond0 { (vlc_val & 0xF) as u16 } else { 0 };
            vlc_val = vlc.advance64(if cond0 { 4 } else { 0 });
            u_q0 = u_q0.wrapping_add(u_ext << 2);
            scratch[sp + 1] = u_q0.wrapping_add(1); // kappa = 1
            let cond1 = i32::from(u_q1) - i32::from(u_bias >> 2) > 32;
            u_ext = if cond1 { (vlc_val & 0xF) as u16 } else { 0 };
            vlc.advance64(if cond1 { 4 } else { 0 });
            u_q1 = u_q1.wrapping_add(u_ext << 2);
            scratch[sp + 3] = u_q1.wrapping_add(1); // kappa = 1
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
                vlc_val = vlc.fetch64();

                // decode VLC using the context c_q and the head of the VLC bitstream
                let mut t0 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F) as u32) as usize];

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
                vlc_val = vlc.advance64(u32::from(t0) & 0x7);

                // second quad
                let mut t1 = t.vlc_tbl1[(c_q + (vlc_val & 0x7F) as u32) as usize];

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
                vlc_val = vlc.advance64(u32::from(t1) & 0x7);

                // decode u
                // uvlc_mode is made up of u_offset bits from the quad pair
                let uvlc_mode = ((u32::from(t0) & 0x8) << 3) | ((u32::from(t1) & 0x8) << 4);
                let mut uvlc_entry =
                    u32::from(t.uvlc_tbl1[(uvlc_mode + (vlc_val & 0x3F) as u32) as usize]);
                // remove total prefix length
                vlc_val = vlc.advance64(uvlc_entry & 0x7);
                uvlc_entry >>= 3;
                // extract suffixes for quad 0 and 1
                let mut len = uvlc_entry & 0xF; // suffix length for 2 quads
                let tmp = (vlc_val & u64::from(low_mask(len))) as u32; // suffix value for 2 quads
                vlc_val = vlc.advance64(len);
                uvlc_entry >>= 4;
                // quad 0 length
                len = uvlc_entry & 0x7; // quad 0 suffix length
                uvlc_entry >>= 3;
                let mut u_q0 = ((uvlc_entry & 7) + (tmp & !(0xFFu32 << len))) as u16;
                let mut u_q1 = ((uvlc_entry >> 3) + (tmp >> len)) as u16; // u_q

                // decode u_q extensions, which is needed only when bitplane_count > 32
                let cond0 = u_q0 > 32;
                let mut u_ext = if cond0 { (vlc_val & 0xF) as u16 } else { 0 };
                vlc_val = vlc.advance64(if cond0 { 4 } else { 0 });
                u_q0 = u_q0.wrapping_add(u_ext << 2);
                scratch[sp + 1] = u_q0;
                let cond1 = u_q1 > 32;
                u_ext = if cond1 { (vlc_val & 0xF) as u16 } else { 0 };
                vlc.advance64(if cond1 { 4 } else { 0 });
                u_q1 = u_q1.wrapping_add(u_ext << 2);
                scratch[sp + 3] = u_q1;
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
        let mut v_n_scratch = [0u64; V_N_SIZE]; // 4+ kB

        let mut magsgn = Frwd::<0xFF>::init8(coded_data, lcup - scup);

        let mut sp = 0usize;
        let mut vp = 0usize;
        let mut dp = 0usize;

        let mut prev_v_n = 0u64;
        let mut x = 0u32;
        while x < width {
            let inf = u32::from(scratch[sp]);
            let u_q = u32::from(scratch[sp + 1]);
            if u_q > mmsbp2 {
                return false;
            }

            // (the first sample's v_n is not kept, as upstream's is overwritten)
            let (val, _) = decode_sample64(&mut magsgn, inf, u_q, 0, p);
            decoded_data[dp] = val;
            let (val, v_n) = decode_sample64(&mut magsgn, inf, u_q, 1, p);
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

            let (val, _) = decode_sample64(&mut magsgn, inf, u_q, 2, p);
            decoded_data[dp] = val;
            let (val, v_n) = decode_sample64(&mut magsgn, inf, u_q, 3, p);
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
                let emax = 63 - count_leading_zeros64(2 | v_n_scratch[vp] | v_n_scratch[vp + 1]); // emax-1
                let kappa = if gamma != 0 { emax } else { 1 };

                let u_q = u_q + kappa;
                if u_q > mmsbp2 {
                    return false;
                }

                // (the first sample's v_n is not kept, as upstream's is overwritten)
                let (val, _) = decode_sample64(&mut magsgn, inf, u_q, 0, p);
                decoded_data[dp] = val;
                let (val, v_n) = decode_sample64(&mut magsgn, inf, u_q, 1, p);
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

                let (val, _) = decode_sample64(&mut magsgn, inf, u_q, 2, p);
                decoded_data[dp] = val;
                let (val, v_n) = decode_sample64(&mut magsgn, inf, u_q, 3, p);
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
                            // sign and bin center; upstream's `3u << (p - 2)` shifts a 32-bit
                            // value, which x86-64 takes modulo 32 (docs/improvements.md, I-25)
                            let val = u64::from(3u32.wrapping_shl(p - 2));
                            col_mask = 0xF;
                            for dp in (dpp + x as usize..).take(4) {
                                if (col_mask & new_sig) != 0 {
                                    // scan 4 signs
                                    let mut sample_mask = 0x1111u32 & col_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp] = (u64::from(cwd) << 63) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + stride] = (u64::from(cwd) << 63) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + 2 * stride] =
                                            (u64::from(cwd) << 63) | val;
                                        cwd >>= 1;
                                        cnt += 1;
                                    }

                                    sample_mask += sample_mask;
                                    if new_sig & sample_mask != 0 {
                                        decoded_data[dp + 3 * stride] =
                                            (u64::from(cwd) << 63) | val;
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
                let half = 1u64 << (p - 2);
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
                                        let mut sym = u64::from(cwd & 1); // get it value
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
