// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_encoder.h` (src/lib/OpenEXRCore/internal_dwa_encoder.h:1-1249
//! @ v3.5.3): the LOSSY_DCT encoder, of one channel or of a set of three color converted
//! together, with its quantizer.
//!
//! The quantizer counts bits with `__builtin_popcount` where the compiler has it (GCC, the
//! Linux wheel) and with a multiply trick under MSVC (the Windows wheel), which has no
//! `__has_builtin` and whose trick drops bit 15: the two wheels can quantize a coefficient
//! differently (docs/improvements.md, I-22), and the port follows each.

use crate::context::Error;
use crate::errors::ErrorCode;
use crate::internal_coding::{float_to_half, half_to_float};
use crate::internal_dwa_channeldata::DctCoderChannelData;
use crate::internal_dwa_classifier::PIXEL_FLOAT;
use crate::internal_dwa_simd::{Block, csc709_forward64, dct_forward_8x8};
use crate::internal_xdr::{one_from_native16, one_to_native16, unaligned_load32};

fn bad() -> Error {
    Error::bare(ErrorCode::InvalidArgument)
}

/// `jpegQuantTableY` and its smallest value
/// (src/lib/OpenEXRCore/internal_dwa_encoder.h:149-155 @ v3.5.3).
const JPEG_QUANT_TABLE_Y: [i32; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const JPEG_QUANT_TABLE_Y_MIN: i32 = 10;

/// `jpegQuantTableCbCr` and its smallest value
/// (src/lib/OpenEXRCore/internal_dwa_encoder.h:157-163 @ v3.5.3).
const JPEG_QUANT_TABLE_CBCR: [i32; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];
const JPEG_QUANT_TABLE_CBCR_MIN: i32 = 17;

/// Port of `LossyDctEncoder` (src/lib/OpenEXRCore/internal_dwa_encoder.h:52-84 @ v3.5.3),
/// with `LossyDctEncoder_base_construct` (lines 126-203): the AC and DC buffers from where
/// this encoder starts.
#[derive(Debug)]
pub(crate) struct LossyDctEncoder<'a> {
    to_nonlinear: Option<&'a [u16]>,
    pub(crate) num_ac_comp: u64,
    pub(crate) num_dc_comp: u64,
    width: i32,
    height: i32,
    packed_ac: &'a mut [u16],
    packed_dc: &'a mut [u8],
    quant_table_y: [f32; 64],
    hquant_table_y: [u16; 64],
    quant_table_cbcr: [f32; 64],
    hquant_table_cbcr: [u16; 64],
}

impl<'a> LossyDctEncoder<'a> {
    /// `LossyDctEncoder_construct` and `LossyDctEncoderCsc_construct`
    /// (src/lib/OpenEXRCore/internal_dwa_encoder.h:211-260 @ v3.5.3), whose channels are
    /// given to [`Self::execute`]. "We take the generic JPEG quantization tables and
    /// normalize them by the smallest component in each table."
    pub(crate) fn new(
        quant_base_error: f32,
        packed_ac: &'a mut [u16],
        packed_dc: &'a mut [u8],
        to_nonlinear: Option<&'a [u16]>,
        width: i32,
        height: i32,
    ) -> Self {
        let mut qbe = quant_base_error;
        if qbe < 0.0 {
            qbe = 0.0;
        }
        let mut e = LossyDctEncoder {
            to_nonlinear,
            num_ac_comp: 0,
            num_dc_comp: 0,
            width,
            height,
            packed_ac,
            packed_dc,
            quant_table_y: [0.0; 64],
            hquant_table_y: [0; 64],
            quant_table_cbcr: [0.0; 64],
            hquant_table_cbcr: [0; 64],
        };
        for idx in 0..64 {
            e.quant_table_y[idx] =
                qbe * (JPEG_QUANT_TABLE_Y[idx] as f32) / (JPEG_QUANT_TABLE_Y_MIN as f32);
            e.hquant_table_y[idx] = float_to_half(e.quant_table_y[idx]);

            e.quant_table_cbcr[idx] =
                qbe * (JPEG_QUANT_TABLE_CBCR[idx] as f32) / (JPEG_QUANT_TABLE_CBCR_MIN as f32);
            e.hquant_table_cbcr[idx] = float_to_half(e.quant_table_cbcr[idx]);
        }
        e
    }

    /// Port of `LossyDctEncoder_execute` (src/lib/OpenEXRCore/internal_dwa_encoder.h:958-1142
    /// @ v3.5.3): the channels' rows of `packed` (offsets in their `rows`) cut into 8x8
    /// blocks (mirrored at the edges), made nonlinear, color converted for a set of three,
    /// transformed, quantized and zig-zagged; the DC values go to the DC buffer, one plane
    /// per channel, and the AC values, run-length coded, to the AC buffer.
    pub(crate) fn execute(
        &mut self,
        chan_data: &mut [&mut DctCoderChannelData],
        packed: &[u8],
    ) -> Result<(), Error> {
        let num_comp = chan_data.len();
        let num_blocks_x = ((self.width as f32) / 8.0f32).ceil() as i32;
        let num_blocks_y = ((self.height as f32) / 8.0f32).ceil() as i32;
        let width = self.width.max(0) as usize;

        let mut half_zig_coef = [0u16; 64];
        let mut curr_ac_comp = 0usize;

        self.num_ac_comp = 0;
        self.num_dc_comp = 0;

        // A temp half buffer to quantize into for any FLOAT source channels: run over all
        // the float scanlines, quantizing, and re-assigning _rowPtr[y]. We need to translate
        // FLOAT XDR to HALF XDR.
        let mut tmp_half: Vec<Option<Vec<u16>>> = vec![None; num_comp];
        for (chan, cd) in chan_data.iter().enumerate() {
            if cd.type_ != PIXEL_FLOAT {
                continue;
            }
            let mut buf = vec![0u16; width * self.height.max(0) as usize];
            for y in 0..self.height.max(0) as usize {
                let src_xdr = *cd.rows.get(y).ok_or_else(bad)?;
                let row = packed.get(src_xdr..src_xdr + 4 * width).ok_or_else(bad)?;
                for x in 0..width {
                    // Clamp to half ranges, instead of just casting. This avoids introducing
                    // Infs which end up getting zeroed later
                    let bits = unaligned_load32(&row[4 * x..]);
                    let mut src = f32::from_bits(bits);
                    if src > 65504.0f32 {
                        src = 65504.0f32;
                    } else if src < -65504.0f32 {
                        src = -65504.0f32;
                    }
                    buf[y * width + x] = one_from_native16(float_to_half(src));
                }
            }
            tmp_half[chan] = Some(buf);
        }

        // Pack DC components together by common plane, so we can get a little more out of
        // differencing them. We'll always have one component per block, so we can computed
        // offsets.
        let plane = (num_blocks_x as i64 * num_blocks_y as i64).max(0) as usize;
        let mut dc_comp: [usize; 3] = [0, plane, 2 * plane];

        for blocky in 0..num_blocks_y {
            for blockx in 0..num_blocks_x {
                for (chan, cd) in chan_data.iter_mut().enumerate() {
                    // Break the source into 8x8 blocks. If we don't fit at the edges, mirror.
                    // Also, convert from linear to nonlinear representation. Our source is
                    // XDR, so convert it to NATIVE first; the lookup table maps NATIVE halves
                    // to NATIVE halves.
                    for y in 0..8 {
                        for x in 0..8 {
                            let mut vx = 8 * blockx + x;
                            let mut vy = 8 * blocky + y;

                            if vx >= self.width {
                                vx = self.width - (vx - (self.width - 1));
                            }
                            if vx < 0 {
                                vx = self.width - 1;
                            }
                            if vy >= self.height {
                                vy = self.height - (vy - (self.height - 1));
                            }
                            if vy < 0 {
                                vy = self.height - 1;
                            }

                            let (vx, vy) = (vx as usize, vy as usize);
                            let mut h = match &tmp_half[chan] {
                                Some(buf) => buf[vy * width + vx],
                                None => {
                                    let row = *cd.rows.get(vy).ok_or_else(bad)?;
                                    let at = row + 2 * vx;
                                    let b = packed.get(at..at + 2).ok_or_else(bad)?;
                                    u16::from_le_bytes([b[0], b[1]])
                                }
                            };

                            h = one_to_native16(h);
                            if let Some(lut) = self.to_nonlinear {
                                h = lut[usize::from(h)];
                            }

                            cd.dct_data[(y * 8 + x) as usize] = half_to_float(h);
                        }
                    }
                }

                // Color space conversion
                if num_comp == 3 {
                    let [r, g, b] = chan_data else {
                        unreachable!("three channels")
                    };
                    csc709_forward64(&mut r.dct_data, &mut g.dct_data, &mut b.dct_data);
                }

                for (chan, cd) in chan_data.iter_mut().enumerate() {
                    let (quant_table, hquant_table) = if chan == 0 {
                        (&self.quant_table_y, &self.hquant_table_y)
                    } else {
                        (&self.quant_table_cbcr, &self.hquant_table_cbcr)
                    };

                    // Forward DCT
                    dct_forward_8x8(&mut cd.dct_data);

                    // Quantize to half, zigzag, and convert to XDR
                    quantize_coeff_and_zig_xdr(
                        &mut half_zig_coef,
                        &cd.dct_data,
                        quant_table,
                        hquant_table,
                    );

                    // Save the DC component separately, to be compressed on its own.
                    let at = 2 * dc_comp[chan];
                    self.packed_dc
                        .get_mut(at..at + 2)
                        .ok_or_else(bad)?
                        .copy_from_slice(&half_zig_coef[0].to_le_bytes());
                    dc_comp[chan] += 1;
                    self.num_dc_comp += 1;

                    // Then RLE the AC components (which will record the count of the
                    // resulting number of items)
                    self.rle_ac(&half_zig_coef, &mut curr_ac_comp)?;
                }
            }
        }
        Ok(())
    }

    /// Port of `LossyDctEncoder_rleAc` (src/lib/OpenEXRCore/internal_dwa_encoder.h:1169-1249
    /// @ v3.5.3): the block's AC values into the AC buffer, a run of zeros written as
    /// `0xff00 | length` and a run to the end of the block as `0xff00`.
    fn rle_ac(&mut self, block: &[u16; 64], ac_ptr: &mut usize) -> Result<(), Error> {
        let mut dct_comp = 1usize;
        let rle_symbol: u16 = 0x0;
        let mut cur_ac = *ac_ptr;
        let mut put = |cur: &mut usize, v: u16| -> Result<(), Error> {
            *self.packed_ac.get_mut(*cur).ok_or_else(bad)? = v;
            *cur += 1;
            Ok(())
        };
        let mut count = 0u64;

        while dct_comp < 64 {
            let mut run_len = 1usize;

            // If we don't have a 0, output verbatim
            if block[dct_comp] != rle_symbol {
                put(&mut cur_ac, block[dct_comp])?;
                count += 1;
                dct_comp += run_len;
                continue;
            }

            // We're sitting on a 0, so see how big the run is.
            while dct_comp + run_len < 64 && block[dct_comp + run_len] == rle_symbol {
                run_len += 1;
            }

            // If the run len is too small, just output verbatim otherwise output our run
            // token. Using 0xff00 for "end of block" can save a bit of space.
            if run_len == 1 {
                put(&mut cur_ac, block[dct_comp])?;
            } else if run_len + dct_comp == 64 {
                // Signal EOB
                put(&mut cur_ac, one_from_native16(0xff00))?;
            } else {
                // Signal normal run
                put(&mut cur_ac, one_from_native16(0xff00 | run_len as u16))?;
            }
            count += 1;

            // Advance by runLen
            dct_comp += run_len;
        }
        self.num_ac_comp += count;
        *ac_ptr = cur_ac;
        Ok(())
    }
}

/// `countSetBits` (src/lib/OpenEXRCore/internal_dwa_encoder.h:266-288 @ v3.5.3):
/// `__builtin_popcount` of the value as a `uint16_t`, or under MSVC the multiply trick, which
/// drops bit 15.
fn count_set_bits(x: u32) -> i32 {
    if cfg!(target_env = "msvc") {
        let mut y = u64::from(x).wrapping_mul(0x0002_0004_0008_0010);
        y &= 0x1111_1111_1111_1111;
        y = y.wrapping_mul(0x1111_1111_1111_1111);
        y >>= 60;
        y as i32
    } else {
        (x as u16).count_ones() as i32
    }
}

/// `countLeadingZeros` (src/lib/OpenEXRCore/internal_dwa_encoder.h:290-307 @ v3.5.3):
/// `__builtin_clz` of the value as a `uint16_t` (`31 - _BitScanReverse` under MSVC); never
/// called with 0.
fn count_leading_zeros(src: u32) -> u32 {
    u32::from(src as u16).leading_zeros()
}

/// The state of `TEST_QUANT_ALTERNATE_LARGE` and `_SMALL`
/// (src/lib/OpenEXRCore/internal_dwa_encoder.h:331-373 @ v3.5.3).
struct Alternates {
    smallest: u32,
    smallbits: i32,
    smalldelta: f32,
    err_tol: f32,
    src_float: f32,
}

impl Alternates {
    fn new(abssrc: u32, err_tol: f32, src_float: f32) -> Self {
        Alternates {
            smallest: abssrc,
            smallbits: count_set_bits(abssrc),
            smalldelta: err_tol,
            err_tol,
            src_float,
        }
    }

    fn test(&mut self, alt: u32, delta_of: impl Fn(f32) -> f32) {
        let bits = count_set_bits(alt);
        if bits < self.smallbits {
            let delta = delta_of(half_to_float(alt as u16));
            if delta < self.err_tol {
                self.smallbits = bits;
                self.smalldelta = delta;
                self.smallest = alt;
            }
        } else if bits == self.smallbits {
            let delta = delta_of(half_to_float(alt as u16));
            if delta < self.smalldelta {
                self.smallest = alt;
                self.smalldelta = delta;
                self.smallbits = bits;
            }
        }
    }

    /// `TEST_QUANT_ALTERNATE_LARGE`: `alt` above the source.
    fn large(&mut self, alt: u32) {
        let s = self.src_float;
        self.test(alt, |a| a - s);
    }

    /// `TEST_QUANT_ALTERNATE_SMALL`: `alt` below the source.
    fn small(&mut self, alt: u32) {
        let s = self.src_float;
        self.test(alt, |a| s - a);
    }
}

/// `npow2`, `lowermask`, `mask` and `mask2` of the handlers.
fn npow2_masks(tol_sig: u32) -> (u32, u32, u32, u32) {
    let tsigshift = 32 - count_leading_zeros(tol_sig);
    let npow2 = 1u32 << tsigshift;
    let lowermask = npow2.wrapping_sub(1);
    let mask = !lowermask;
    let mask2 = mask ^ npow2;
    (npow2, lowermask, mask, mask2)
}

/// `mask3`: `mask2 ^ (((npow2 << 1) * extrabit) | ((npow2 >> 1) * !extrabit))`.
fn mask3_of(mask2: u32, npow2: u32, extrabit: u32) -> u32 {
    mask2 ^ (((npow2 << 1) * extrabit) | ((npow2 >> 1) * u32::from(extrabit == 0)))
}

/// Port of `handleQuantizeDenormTol` (src/lib/OpenEXRCore/internal_dwa_encoder.h:376-395
/// @ v3.5.3).
fn handle_quantize_denorm_tol(abssrc: u32, tol_sig: u32, err_tol: f32, src_float: f32) -> u32 {
    let (npow2, _, mask, mask2) = npow2_masks(tol_sig);
    let mut a = Alternates::new(abssrc, err_tol, src_float);
    a.small(abssrc & mask2);
    a.small(abssrc & mask);
    a.large(abssrc.wrapping_add(npow2) & mask);
    a.large(abssrc.wrapping_add(npow2 << 1) & mask);
    a.smallest
}

/// Port of `handleQuantizeGeneric` (src/lib/OpenEXRCore/internal_dwa_encoder.h:397-442
/// @ v3.5.3).
fn handle_quantize_generic(abssrc: u32, tol_sig: u32, err_tol: f32, src_float: f32) -> u32 {
    let (npow2, lowermask, mask, mask2) = npow2_masks(tol_sig);
    let src_masked_val = abssrc & lowermask;
    let extrabit = u32::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    if extrabit != 0 {
        a.small(abssrc & mask3);
        a.small(abssrc & mask2);
        a.small(abssrc & mask);
    } else if (abssrc & npow2) != 0 {
        a.small(abssrc & mask2);
        a.small(abssrc & mask3);
        a.small(abssrc & mask);
    } else {
        a.small(abssrc & mask2);
        a.small(abssrc & mask);
        a.small(abssrc & mask3);
    }
    a.large(abssrc.wrapping_add(npow2) & mask);
    a.smallest
}

/// Port of `handleQuantizeEqualExp` (src/lib/OpenEXRCore/internal_dwa_encoder.h:447-484
/// @ v3.5.3).
fn handle_quantize_equal_exp(abssrc: u32, tol_sig: u32, err_tol: f32, src_float: f32) -> u32 {
    let npow2: u32 = 0x0800;
    let lowermask = npow2 - 1;
    let mask = !lowermask;
    let mask2 = mask ^ npow2;

    let src_masked_val = abssrc & lowermask;
    let extrabit = u32::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    // doing in this order mask2, mask, +npow guarantees sorting of values so can avoid a
    // couple of conditionals in the macros
    if src_masked_val == abssrc {
        a.small(abssrc & mask3);
    } else {
        let mut alt0 = abssrc & mask2;
        let alt1 = abssrc & mask;
        if alt0 == alt1 {
            alt0 = abssrc & mask3;
        }
        a.small(alt0);
        a.small(alt1);
    }
    a.large(abssrc.wrapping_add(npow2) & mask);
    a.smallest
}

/// Port of `handleQuantizeCloseExp` (src/lib/OpenEXRCore/internal_dwa_encoder.h:486-547
/// @ v3.5.3).
fn handle_quantize_close_exp(abssrc: u32, tol_sig: u32, err_tol: f32, src_float: f32) -> u32 {
    let npow2: u32 = 0x0400;
    let lowermask = npow2 - 1;
    let mask = !lowermask;
    let mask2 = mask ^ npow2;

    let src_masked_val = abssrc & lowermask;
    let extrabit = u32::from(tol_sig > src_masked_val);
    let mask3 = mask3_of(mask2, npow2, extrabit);

    let mut alternates = [0u32; 3];
    if (abssrc & npow2) == 0 {
        // by definition, src&mask2 == src&mask
        if extrabit != 0 {
            alternates[0] = abssrc & mask3;
            alternates[1] = abssrc & mask;
        } else {
            alternates[0] = abssrc & mask;
            alternates[1] = abssrc & mask3;
        }
    } else if extrabit != 0 {
        alternates[0] = abssrc & mask3;
        alternates[1] = abssrc & mask2;
        let alt1delta = src_float - half_to_float(alternates[1] as u16);
        if alt1delta >= err_tol {
            alternates[1] = abssrc & mask;
        }
    } else {
        alternates[0] = abssrc & mask2;
        alternates[1] = abssrc & mask3;
        let alt0delta = src_float - half_to_float(alternates[0] as u16);
        if alt0delta >= err_tol {
            alternates[0] = abssrc & mask;
        }
    }
    alternates[2] = abssrc.wrapping_add(npow2) & mask;

    let mut a = Alternates::new(abssrc, err_tol, src_float);
    a.small(alternates[0]);
    a.small(alternates[1]);
    a.large(alternates[2]);
    a.smallest
}

/// The search after the two candidates of `handleQuantizeLargerSig` and
/// `handleQuantizeSmallerSig` (src/lib/OpenEXRCore/internal_dwa_encoder.h:549-660 @ v3.5.3),
/// which is the same code in both.
fn handle_two_candidates(abssrc: u32, alt0: u32, alt1: u32, err_tol: f32, src_float: f32) -> u32 {
    let bits0 = count_set_bits(alt0);
    let bits1 = count_set_bits(alt1);

    if bits1 < bits0 {
        let delta = half_to_float(alt1 as u16) - src_float; // alt1 >= srcFloat
        // bits1 < bits0 and if ok, just return
        if delta < err_tol {
            return alt1;
        }
        let delta = src_float - half_to_float(alt0 as u16); // alt0 <= srcFloat
        if delta < err_tol {
            return alt0;
        }
    } else if bits1 == bits0 {
        let delta = src_float - half_to_float(alt0 as u16);
        let delta1 = half_to_float(alt1 as u16) - src_float;
        if delta < err_tol {
            return if delta1 < delta { alt1 } else { alt0 };
        }
        if delta1 < err_tol {
            return alt1;
        }
    } else {
        let delta = src_float - half_to_float(alt0 as u16);
        // bits0 < bits1 so if ok, just return
        if delta < err_tol {
            return alt0;
        }
        // fallback... in this case, alt1 rounding could have made bits1 larger than src,
        // test for that
        let srcbits = count_set_bits(abssrc);
        if bits1 < srcbits {
            let delta = half_to_float(alt1 as u16) - src_float;
            if delta < err_tol {
                return alt1;
            }
        }
    }
    abssrc
}

/// Port of `handleQuantizeLargerSig` (src/lib/OpenEXRCore/internal_dwa_encoder.h:549-605
/// @ v3.5.3): "can't fully zero out the masked region, so go to "0.5" of that region and then
/// test the rounded value".
fn handle_quantize_larger_sig(
    abssrc: u32,
    npow2: u32,
    mask: u32,
    err_tol: f32,
    src_float: f32,
) -> u32 {
    let mask2 = mask ^ (npow2 | (npow2 >> 1));
    let alt0 = abssrc & mask2;
    let alt1 = abssrc.wrapping_add(npow2) & mask;
    handle_two_candidates(abssrc, alt0, alt1, err_tol, src_float)
}

/// Port of `handleQuantizeSmallerSig` (src/lib/OpenEXRCore/internal_dwa_encoder.h:607-660
/// @ v3.5.3): base truncation and rounded truncation.
fn handle_quantize_smaller_sig(
    abssrc: u32,
    npow2: u32,
    mask: u32,
    err_tol: f32,
    src_float: f32,
) -> u32 {
    let alt0 = abssrc & mask;
    let alt1 = abssrc.wrapping_add(npow2) & mask;
    handle_two_candidates(abssrc, alt0, alt1, err_tol, src_float)
}

/// Port of `handleQuantizeEqualSig` (src/lib/OpenEXRCore/internal_dwa_encoder.h:662-720
/// @ v3.5.3).
fn handle_quantize_equal_sig(
    abssrc: u32,
    npow2: u32,
    mask: u32,
    err_tol: f32,
    src_float: f32,
) -> u32 {
    let mut alt0 = abssrc & mask;
    let alt1 = abssrc.wrapping_add(npow2) & mask;

    let mut delta0 = src_float - half_to_float(alt0 as u16);
    if delta0 >= err_tol {
        let mask2 = mask ^ (npow2 | (npow2 >> 1));
        alt0 = abssrc & mask2;
        delta0 = src_float - half_to_float(alt0 as u16);

        // avoid a re-check against the tolerance below...
        if delta0 >= err_tol {
            let delta1 = half_to_float(alt1 as u16) - src_float;
            if delta1 < err_tol {
                let bits1 = count_set_bits(alt1);
                let srcbits = count_set_bits(abssrc);
                if bits1 < srcbits {
                    return alt1;
                }
            }
            return abssrc;
        }
    }

    let bits0 = count_set_bits(alt0);
    let bits1 = count_set_bits(alt1);

    if bits1 < bits0 {
        let delta1 = half_to_float(alt1 as u16) - src_float;
        // bits1 < bits0 and if ok, just return
        if delta1 < err_tol {
            return alt1;
        }
    } else if bits1 == bits0 {
        let delta1 = half_to_float(alt1 as u16) - src_float;
        if delta1 < delta0 {
            return alt1;
        }
    }

    // bits0 < bits1 and ok or alt1 failed
    alt0
}

/// Port of `handleQuantizeDefault` (src/lib/OpenEXRCore/internal_dwa_encoder.h:722-740
/// @ v3.5.3).
fn handle_quantize_default(abssrc: u32, tol_sig: u32, err_tol: f32, src_float: f32) -> u32 {
    let (npow2, lowermask, mask, _) = npow2_masks(tol_sig);
    let src_masked_val = abssrc & lowermask;

    if src_masked_val > tol_sig {
        return handle_quantize_larger_sig(abssrc, npow2, mask, err_tol, src_float);
    } else if src_masked_val < tol_sig {
        return handle_quantize_smaller_sig(abssrc, npow2, mask, err_tol, src_float);
    }
    handle_quantize_equal_sig(abssrc, npow2, mask, err_tol, src_float)
}

/// Port of `algoQuantize` (src/lib/OpenEXRCore/internal_dwa_encoder.h:742-891 @ v3.5.3): the
/// half with the fewest bits set within `errTol` of `src` (`herrTol` is `errTol` as a half),
/// found from the bits of the tolerance shifted to the source's exponent.
fn algo_quantize(src: u32, herr_tol: u32, err_tol: f32, src_float: f32) -> u16 {
    let sign = src & 0x8000;
    let abssrc = src & 0x7FFF;

    let src_float = src_float.abs();

    let src_exp_biased = src & 0x7C00;
    let tol_exp_biased = herr_tol & 0x7C00;

    // if nan / inf, just bail and return src
    if src_exp_biased == 0x7C00 {
        return src as u16;
    }

    // can't possibly beat 0 bits
    if src_float < err_tol {
        return 0;
    }

    // (the tolerance's exponent is never above the source's here: a half at least errTol
    // is at least errTol rounded to a half; the shifts are x86's, by the count's low bits)
    let exp_diff = src_exp_biased.wrapping_sub(tol_exp_biased) >> 10;
    let mut tol_sig = ((herr_tol & 0x3FF) | (1 << 10)).wrapping_shr(exp_diff);

    if tol_exp_biased == 0 {
        if exp_diff == 0 || exp_diff == 1 {
            tol_sig = herr_tol & 0x3FF;
            if tol_sig == 0 {
                return src as u16;
            }
            return (sign | handle_quantize_generic(abssrc, tol_sig, err_tol, src_float)) as u16;
        }

        tol_sig = herr_tol & 0x3FF;
        if tol_sig == 0 {
            return src as u16;
        }

        tol_sig = tol_sig.wrapping_shr(exp_diff);
        if tol_sig == 0 {
            tol_sig = 1;
        }

        return (sign | handle_quantize_denorm_tol(abssrc, tol_sig, err_tol, src_float)) as u16;
    }

    if tol_sig == 0 {
        return src as u16;
    }

    // first, handle the default case of a 'large' diff between src and tolerance, or a
    // denorm src
    if exp_diff > 1 || src_exp_biased == 0 {
        return (sign | handle_quantize_default(abssrc, tol_sig, err_tol, src_float)) as u16;
    }

    if exp_diff == 0 {
        return (sign | handle_quantize_equal_exp(abssrc, tol_sig, err_tol, src_float)) as u16;
    }
    (sign | handle_quantize_close_exp(abssrc, tol_sig, err_tol, src_float)) as u16
}

/// `inv_remap` of `quantizeCoeffAndZigXDR`: where each coefficient goes in zig-zag order
/// (src/lib/OpenEXRCore/internal_dwa_encoder.h:907-911 @ v3.5.3).
const INV_REMAP: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11,
    18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34,
    37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];

/// Port of `quantizeCoeffAndZigXDR` (src/lib/OpenEXRCore/internal_dwa_encoder.h:893-947
/// @ v3.5.3): each coefficient to a half (Imath's conversion), quantized within its
/// tolerance, in zig-zag order and XDR.
fn quantize_coeff_and_zig_xdr(
    half_zig_coeff: &mut [u16; 64],
    dctvals: &Block,
    tolerances: &[f32; 64],
    halftols: &[u16; 64],
) {
    for i in 0..64 {
        let src = float_to_half(dctvals[i]);
        let q = algo_quantize(
            u32::from(src),
            u32::from(halftols[i]),
            tolerances[i],
            half_to_float(src),
        );
        half_zig_coeff[INV_REMAP[i]] = one_from_native16(q);
    }
}
