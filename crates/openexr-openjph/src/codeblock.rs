// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_codeblock.cpp` (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:1-268
//! @ OpenJPH 0.32.0) and `ojph_codeblock.h`
//! (external/OpenJPH/src/core/codestream/ojph_codeblock.h:1-129 @ OpenJPH 0.32.0), the decoding
//! side: a codeblock's samples, decoded from its coded data and pulled out line by line.

use crate::base::Size;
use crate::bitbuffer_read::PREFIX_BUF_SIZE;
use crate::block_decoder32::ojph_decode_codeblock32;
use crate::block_decoder64::ojph_decode_codeblock64;
use crate::codeblock_fun::CodeblockFun;
use crate::mem::{LFT_INTEGER, LineBuf, LineData};
use crate::message::{OjphError, OjphResult, error, info};

const FILE: &str = "ojph_codeblock.cpp";

/// `codeblock::BUF32` and `BUF64`.
const BUF32: u32 = 4;
const BUF64: u32 = 8;

/// Port of `coded_cb_header` (external/OpenJPH/src/core/codestream/ojph_codeblock.h:113-125
/// @ OpenJPH 0.32.0): a codeblock's passes and coded data. `next_coded` holds the data with
/// `PREFIX_BUF_SIZE` bytes before it and `SUFFIX_BUF_SIZE` after.
#[derive(Debug, Clone, Default)]
pub(crate) struct CodedCbHeader {
    pub(crate) pass_length: [u32; 2],
    pub(crate) num_passes: u32,
    pub(crate) kmax: u32,
    pub(crate) missing_msbs: u32,
    pub(crate) next_coded: Option<Vec<u8>>,
}

/// Port of `codeblock` (external/OpenJPH/src/core/codestream/ojph_codeblock.h:58-110
/// @ OpenJPH 0.32.0). `coded_cb` is the index of its header in the subband's.
#[derive(Debug, Clone, Default)]
pub(crate) struct Codeblock {
    precision: u32,
    buf32: Vec<u32>,
    buf64: Vec<u64>,
    nominal_size: Size,
    cb_size: Size,
    stride: u32,
    buf_size: u32,
    line_offset: u32,
    cur_line: u32,
    delta: f32,
    delta_inv: f32,
    k_max: u32,
    reversible: bool,
    resilient: bool,
    stripe_causal: bool,
    zero_block: bool,
    max_val64: [u64; 4],
    pub(crate) coded_cb: usize,
    codeblock_functions: CodeblockFun,
}

impl Codeblock {
    /// Port of `codeblock::finalize_alloc`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:72-112 @ OpenJPH 0.32.0). The
    /// buffer has a row more than upstream's: the block decoders write the row after an odd
    /// last row, which upstream leaves to the next allocation (docs/improvements.md, U-17).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finalize_alloc(
        delta: f32,
        nominal: Size,
        cb_size: Size,
        coded_cb: usize,
        k_max: u32,
        line_offset: u32,
        precision: u32,
        reversible: bool,
        resilient: bool,
        stripe_causal: bool,
    ) -> Self {
        let f = 64 / 4 - 1;
        let stride = (nominal.w + f) & !f; // a multiple of 8
        let buf_size = stride * nominal.h;
        let spare = (buf_size + stride) as usize;
        let (precision, buf32, buf64) = if precision <= 32 {
            (BUF32, vec![0u32; spare], Vec::new())
        } else {
            (BUF64, Vec::new(), vec![0u64; spare])
        };
        Codeblock {
            precision,
            buf32,
            buf64,
            nominal_size: nominal,
            cb_size,
            stride,
            buf_size,
            line_offset,
            cur_line: 0,
            delta,
            delta_inv: 1.0 / delta,
            k_max,
            reversible,
            resilient,
            stripe_causal,
            zero_block: false,
            max_val64: [0; 4],
            coded_cb,
            codeblock_functions: CodeblockFun::init(reversible),
        }
    }

    /// Port of `codeblock::recreate`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:178-187 @ OpenJPH 0.32.0).
    pub(crate) fn recreate(&mut self, cb_size: Size, coded_cb: usize) {
        debug_assert!(cb_size.h * self.stride <= self.buf_size && cb_size.w <= self.stride);
        self.cb_size = cb_size;
        self.coded_cb = coded_cb;
        self.cur_line = 0;
        self.max_val64 = [0; 4];
        self.zero_block = false;
    }

    /// Port of `codeblock::decode`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:190-226 @ OpenJPH 0.32.0).
    pub(crate) fn decode(&mut self, coded_cb: &CodedCbHeader) -> OjphResult<()> {
        if let Some(buf) = coded_cb
            .next_coded
            .as_ref()
            .filter(|_| coded_cb.pass_length[0] > 0 && coded_cb.num_passes > 0)
        {
            let data = &buf[PREFIX_BUF_SIZE..];
            let result = if self.precision == BUF32 {
                ojph_decode_codeblock32(
                    data,
                    &mut self.buf32,
                    coded_cb.missing_msbs,
                    coded_cb.num_passes,
                    coded_cb.pass_length[0],
                    coded_cb.pass_length[1],
                    self.cb_size.w,
                    self.cb_size.h,
                    self.stride,
                    self.stripe_causal,
                )
            } else {
                ojph_decode_codeblock64(
                    data,
                    &mut self.buf64,
                    coded_cb.missing_msbs,
                    coded_cb.num_passes,
                    coded_cb.pass_length[0],
                    coded_cb.pass_length[1],
                    self.cb_size.w,
                    self.cb_size.h,
                    self.stride,
                    self.stripe_causal,
                )
            };
            if !result {
                if self.resilient {
                    info(0x000300A1, FILE, 217, "Error decoding a codeblock.", &[]);
                    self.zero_block = true;
                } else {
                    return Err(error(
                        0x000300A1,
                        FILE,
                        221,
                        "Error decoding a codeblock.",
                        &[],
                    ));
                }
            }
        } else {
            self.zero_block = true;
        }
        Ok(())
    }

    /// Port of `codeblock::pull_line`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:230-266 @ OpenJPH 0.32.0). With 64-bit buffers, upstream points at the 64-bit sample
    /// `line_offset` of the line, a float line when irreversible, so its floats land at
    /// `2 line_offset` and its clearing covers `2 cb_size.w` floats; the port does the same,
    /// and refuses where that passes the line's storage (docs/improvements.md, U-18).
    pub(crate) fn pull_line(&mut self, line: &mut LineBuf) -> OjphResult<()> {
        let w = self.cb_size.w;
        let sp0 = (self.cur_line * self.stride) as usize;
        let lo = self.line_offset as usize;
        if self.precision == BUF32 {
            if (line.flags & LFT_INTEGER) != 0 {
                let dp = &mut line.i32_mut()[lo..];
                if !self.zero_block {
                    self.codeblock_functions
                        .tx_from_cb32_i(&self.buf32[sp0..], dp, self.k_max, w);
                } else {
                    dp[..w as usize].fill(0);
                }
            } else {
                let dp = &mut line.f32_mut()[lo..];
                if !self.zero_block {
                    self.codeblock_functions
                        .tx_from_cb32_f(&self.buf32[sp0..], dp, self.delta, w);
                } else {
                    dp[..w as usize].fill(0.0);
                }
            }
        } else if let LineData::I64(_) = line.data {
            let dp = &mut line.i64_mut()[lo..];
            if !self.zero_block {
                self.codeblock_functions
                    .tx_from_cb64_i(&self.buf64[sp0..], dp, self.k_max, w);
            } else {
                dp[..w as usize].fill(0);
            }
        } else {
            // a float line, addressed as 64-bit samples
            let dp = line.f32_mut();
            if 2 * lo + 2 * w as usize > dp.len() {
                return Err(OjphError::thrown(
                    "a 64-bit codeblock that passes its float line",
                ));
            }
            let dp = &mut dp[2 * lo..];
            if !self.zero_block {
                self.codeblock_functions
                    .tx_from_cb64_f(&self.buf64[sp0..], dp, self.delta, w);
            } else {
                dp[..2 * w as usize].fill(0.0);
            }
        }
        self.cur_line += 1;
        debug_assert!(self.cur_line <= self.cb_size.h);
        Ok(())
    }
}
