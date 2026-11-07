// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_dwa_decoder.h` (src/lib/OpenEXRCore/internal_dwa_decoder.h:1-782
//! @ v3.5.3): the LOSSY_DCT decoder, of one channel or of a set of three color converted
//! together.
//!
//! The AC and DC streams are XDR (little-endian) 16-bit values, held here as the values the
//! host reads from those bytes on x86-64. Rows are offsets into the chunk's unpacked buffer;
//! a row reaching outside it is refused (docs/improvements.md, U-13).

use crate::context::Error;
use crate::errors::ErrorCode;
use crate::internal_coding::{float_to_half, half_to_float};
use crate::internal_dwa_channeldata::DctCoderChannelData;
use crate::internal_dwa_classifier::PIXEL_FLOAT;
use crate::internal_dwa_simd::{
    DwaFuncs, csc709_inverse, csc709_inverse64, dct_inverse_8x8_dc_only,
};
use crate::internal_xdr::{
    one_from_native16, one_to_native16, unaligned_load16, unaligned_store16, unaligned_store32,
};

fn corrupt() -> Error {
    Error::bare(ErrorCode::CorruptChunk)
}

/// Port of `LossyDctDecoder` (src/lib/OpenEXRCore/internal_dwa_decoder.h:13-53 @ v3.5.3),
/// with `LossyDctDecoder_base_construct` (lines 266-295): the AC stream from where this
/// decoder starts to its end, and the DC stream from where it starts.
#[derive(Debug)]
pub(crate) struct LossyDctDecoder<'a> {
    pub(crate) packed_ac_count: u64,
    pub(crate) packed_dc_count: u64,
    packed_ac: &'a [u16],
    packed_dc: &'a [u16],
    rem_dc_count: u64,
    to_linear: Option<&'a [u16]>,
    width: i32,
    height: i32,
}

impl<'a> LossyDctDecoder<'a> {
    /// `LossyDctDecoder_construct` and `LossyDctDecoderCsc_construct`
    /// (src/lib/OpenEXRCore/internal_dwa_decoder.h:173-264 @ v3.5.3), whose channels are
    /// given to [`Self::execute`].
    pub(crate) fn new(
        packed_ac: &'a [u16],
        packed_dc: &'a [u16],
        rem_dc_count: u64,
        to_linear: Option<&'a [u16]>,
        width: i32,
        height: i32,
    ) -> Self {
        LossyDctDecoder {
            packed_ac_count: 0,
            packed_dc_count: 0,
            packed_ac,
            packed_dc,
            rem_dc_count,
            to_linear,
            width,
            height,
        }
    }

    /// Port of `LossyDctDecoder_unRleAc` (src/lib/OpenEXRCore/internal_dwa_decoder.h:109-169
    /// @ v3.5.3): the AC run-length stream from `*curr_ac_comp` into the zeroed block; returns
    /// the zig-zag index of the last non-zero value (0 for a block of DC only).
    fn un_rle_ac(
        &mut self,
        curr_ac_comp: &mut usize,
        half_zig_block: &mut [u16; 64],
    ) -> Result<i32, Error> {
        let mut dct_comp: i32 = 1;
        let mut ac_comp = *curr_ac_comp;
        let mut lnz = 0;
        let mut ac_count: u64 = 0;

        while dct_comp < 64 {
            let Some(&raw) = self.packed_ac.get(ac_comp) else {
                return Err(corrupt());
            };
            // the AC stream is XDR; test for run symbols on the native value
            // and copy coefficients through unchanged (still XDR)
            let val = one_to_native16(raw);
            if (val & 0xff00) == 0xff00 {
                let count = (val & 0xff) as u8;
                // run, insert 0s - since block pre-zeroed, nothing to do
                // just increment dctComp but test for end of block...
                dct_comp += if count == 0 { 64 } else { i32::from(count) };
            } else {
                // Not a run, just copy over the value
                lnz = dct_comp;
                half_zig_block[dct_comp as usize] = raw;
                dct_comp += 1;
            }
            ac_count += 1;
            ac_comp += 1;
        }

        self.packed_ac_count += ac_count;
        *curr_ac_comp = ac_comp;
        Ok(lnz)
    }

    /// Port of `LossyDctDecoder_execute` (src/lib/OpenEXRCore/internal_dwa_decoder.h:299-780
    /// @ v3.5.3): the channels' blocks, decoded into their rows of `out`. `chan_data` holds one
    /// channel, or the three of a color set (R, G, B).
    pub(crate) fn execute(
        &mut self,
        funcs: &DwaFuncs,
        chan_data: &mut [&mut DctCoderChannelData],
        out: &mut [u8],
    ) -> Result<(), Error> {
        let num_comp = chan_data.len();
        let num_blocks_x = ((i64::from(self.width) + 7) / 8) as i32;
        let num_blocks_y = ((i64::from(self.height) + 7) / 8) as i32;
        let leftover_x = self.width - (num_blocks_x - 1) * 8;
        let leftover_y = self.height - (num_blocks_y - 1) * 8;

        let num_full_blocks_x = self.width / 8;

        let mut curr_ac_comp = 0usize;

        if self.rem_dc_count < (num_comp as u64) * (num_blocks_x as u64) * (num_blocks_y as u64) {
            return Err(corrupt());
        }

        // A temp buffer to hold a rows worth of full 8x8 half-float blocks
        let nbx = num_blocks_x as usize;
        let mut row_block = vec![0u16; num_comp * nbx * 64];

        // Pack DC components together by common plane, so we can get a little more out of
        // differencing them. We'll always have one component per block, so we can computed
        // offsets.
        let plane = nbx * num_blocks_y as usize;
        let mut curr_dc_comp: [usize; 3] = [0, plane, 2 * plane];

        for blocky in 0..num_blocks_y {
            let mut max_y = 8;
            let mut max_x = 8;
            if blocky == num_blocks_y - 1 {
                max_y = leftover_y;
            }

            for blockx in 0..num_blocks_x {
                let mut block_is_constant = true;

                if blockx == num_blocks_x - 1 {
                    max_x = leftover_x;
                }

                // If we can detect that the block is constant values (all components only
                // have DC values, and all AC is 0), we can do everything only on 1 value,
                // instead of all 64.
                for (comp, cd) in chan_data.iter_mut().enumerate() {
                    // DC component is stored separately
                    cd.half_zig_data = [0; 64];
                    cd.half_zig_data[0] =
                        *self.packed_dc.get(curr_dc_comp[comp]).ok_or_else(corrupt)?;
                    curr_dc_comp[comp] += 1;

                    self.packed_dc_count += 1;

                    // UnRLE the AC. This will modify currAcComp
                    let last_non_zero = self.un_rle_ac(&mut curr_ac_comp, &mut cd.half_zig_data)?;

                    // Convert from XDR to NATIVE
                    for v in cd.half_zig_data.iter_mut() {
                        *v = one_to_native16(*v);
                    }

                    if last_non_zero == 0 {
                        // DC only case - AC components are all 0
                        cd.dct_data[0] = half_to_float(cd.half_zig_data[0]);
                        dct_inverse_8x8_dc_only(&mut cd.dct_data);
                    } else {
                        // We have some AC components that are non-zero. Can't use the
                        // 'constant block' optimization
                        block_is_constant = false;

                        // Un-Zig zag
                        funcs.from_half_zig_zag(&cd.half_zig_data, &mut cd.dct_data);

                        // If lastNonZero is less than the first item on each row, we know
                        // that the whole row is zero and can be skipped in the row-oriented
                        // part of the iDCT.
                        let zeroed_rows = if last_non_zero < 2 {
                            7
                        } else if last_non_zero < 3 {
                            6
                        } else if last_non_zero < 9 {
                            5
                        } else if last_non_zero < 10 {
                            4
                        } else if last_non_zero < 20 {
                            3
                        } else if last_non_zero < 21 {
                            2
                        } else if last_non_zero < 35 {
                            1
                        } else {
                            0
                        };
                        funcs.dct_inverse_8x8(&mut cd.dct_data, zeroed_rows);
                    }
                }

                // Perform the CSC
                if num_comp == 3 {
                    let [r, g, b] = chan_data else {
                        unreachable!("three channels")
                    };
                    if !block_is_constant {
                        csc709_inverse64(&mut r.dct_data, &mut g.dct_data, &mut b.dct_data);
                    } else {
                        csc709_inverse(&mut r.dct_data[0], &mut g.dct_data[0], &mut b.dct_data[0]);
                    }
                }

                // Float -> Half conversion. If the block has a constant value, just convert
                // the first pixel.
                for (comp, cd) in chan_data.iter().enumerate() {
                    let at = (comp * nbx + blockx as usize) * 64;
                    let dst: &mut [u16; 64] = (&mut row_block[at..at + 64])
                        .try_into()
                        .expect("a block of 64");
                    if !block_is_constant {
                        funcs.convert_float_to_half64(dst, &cd.dct_data);
                    } else {
                        dst.fill(float_to_half(cd.dct_data[0]));
                    }
                }
            }

            // At this point, we have half-float nonlinear value blocked in rowBlock[][]. We
            // need to unblock the data, transfer back to linear, and write the results in the
            // _rowPtrs[]. (Upstream's SSE2 fast path for the full blocks stores the same
            // values as its scalar one.)
            for (comp, cd) in chan_data.iter().enumerate() {
                let blocks = &row_block[comp * nbx * 64..(comp + 1) * nbx * 64];
                for y in 8 * blocky..8 * blocky + max_y {
                    let row = *cd.rows.get(y as usize).ok_or_else(corrupt)?;
                    let line = (y & 0x7) as usize * 8;
                    let mut put = |x: usize, v: u16| -> Result<(), Error> {
                        let at = row + 2 * x;
                        let dst = out.get_mut(at..at + 2).ok_or_else(corrupt)?;
                        unaligned_store16(dst, one_from_native16(v));
                        Ok(())
                    };

                    // Handle all the full X blocks
                    for blockx in 0..num_full_blocks_x as usize {
                        let src = &blocks[blockx * 64 + line..blockx * 64 + line + 8];
                        for (i, &h) in src.iter().enumerate() {
                            let v = match self.to_linear {
                                Some(lut) => lut[usize::from(h)],
                                None => h,
                            };
                            put(8 * blockx + i, v)?;
                        }
                    }

                    // If we have partial X blocks, deal with all those now
                    if num_full_blocks_x != num_blocks_x {
                        let base = num_full_blocks_x as usize * 64 + line;
                        for x in 0..max_x as usize {
                            let h = blocks[base + x];
                            let v = match self.to_linear {
                                Some(lut) => lut[usize::from(h)],
                                None => h,
                            };
                            put(8 * num_full_blocks_x as usize + x, v)?;
                        }
                    }
                }
            }
        }

        // Walk over all the channels that are of type FLOAT. Convert from HALF XDR back to
        // FLOAT XDR.
        for cd in chan_data.iter() {
            if cd.type_ != PIXEL_FLOAT {
                continue;
            }

            // process in place in reverse to avoid temporary buffer
            for y in 0..self.height.max(0) as usize {
                let row = *cd.rows.get(y).ok_or_else(corrupt)?;
                let row_bytes = out
                    .get_mut(row..row + 4 * self.width.max(0) as usize)
                    .ok_or_else(corrupt)?;
                for x in (0..self.width.max(0) as usize).rev() {
                    let h = unaligned_load16(&row_bytes[2 * x..]);
                    let f = half_to_float(h);
                    unaligned_store32(&mut row_bytes[4 * x..], f.to_bits());
                }
            }
        }

        Ok(())
    }
}
