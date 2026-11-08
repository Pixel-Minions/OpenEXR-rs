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
use crate::block_encoder::{ojph_encode_codeblock32, ojph_encode_codeblock64};
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

    /// Port of `codeblock::push` (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:115-139
    /// @ OpenJPH 0.32.0): a line, converted to sign and magnitude, the maximum kept. With
    /// 64-bit buffers an irreversible codeblock has no conversion (upstream's `tx_to_cb64` is
    /// NULL, and calling it crashes); the port refuses it (docs/improvements.md, U-20).
    pub(crate) fn push(&mut self, line: &LineBuf) -> OjphResult<()> {
        let lo = self.line_offset as usize;
        let dp0 = (self.cur_line * self.stride) as usize;
        let w = self.cb_size.w;
        if self.precision == BUF32 {
            let mut mv = self.max_val64[0] as u32;
            let dp = &mut self.buf32[dp0..];
            if (line.flags & LFT_INTEGER) != 0 {
                self.codeblock_functions.tx_to_cb32_i(
                    &line.i32()[lo..],
                    dp,
                    self.k_max,
                    w,
                    &mut mv,
                );
            } else {
                self.codeblock_functions.tx_to_cb32_f(
                    &line.f32()[lo..],
                    dp,
                    self.delta_inv,
                    w,
                    &mut mv,
                );
            }
            self.max_val64[0] = (self.max_val64[0] & !0xFFFF_FFFF) | u64::from(mv);
        } else {
            if !self.reversible {
                return Err(OjphError::thrown(
                    "an irreversible codeblock of 64-bit samples",
                ));
            }
            let mut mv = self.max_val64[0];
            let dp = &mut self.buf64[dp0..];
            self.codeblock_functions
                .tx_to_cb64_i(&line.i64()[lo..], dp, self.k_max, w, &mut mv);
            self.max_val64[0] = mv;
        }
        self.cur_line += 1;
        Ok(())
    }

    /// Port of `codeblock::encode`
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:142-175 @ OpenJPH 0.32.0): the codeblock coded when a sample is significant.
    pub(crate) fn encode(&mut self, coded_cb: &mut CodedCbHeader) -> OjphResult<()> {
        if self.precision == BUF32 {
            let mv = self.max_val64[0] as u32;
            if mv >= 1u32.wrapping_shl(31u32.wrapping_sub(self.k_max)) {
                coded_cb.missing_msbs = self.k_max - 1;
                coded_cb.num_passes = 1;
                Self::encode_cb32(
                    &self.buf32,
                    self.k_max - 1,
                    1,
                    self.cb_size.w,
                    self.cb_size.h,
                    self.stride,
                    &mut coded_cb.pass_length,
                    &mut coded_cb.next_coded,
                )?;
            }
        } else {
            let mv = self.max_val64[0];
            if mv >= 1u64.wrapping_shl(63u32.wrapping_sub(self.k_max)) {
                coded_cb.missing_msbs = self.k_max - 1;
                coded_cb.num_passes = 1;
                ojph_encode_codeblock64(
                    &self.buf64,
                    self.k_max - 1,
                    1,
                    self.cb_size.w,
                    self.cb_size.h,
                    self.stride,
                    &mut coded_cb.pass_length,
                    &mut coded_cb.next_coded,
                )?;
            }
        }
        Ok(())
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

    /// `encode_cb32` as `codeblock_fun::init` chooses it
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.cpp:225-260 @ OpenJPH 0.32.0):
    /// the AVX-512 encoder at `X86_CPU_EXT_LEVEL_AVX512`, else the generic one (the AVX2
    /// encoder upstream chooses from `X86_CPU_EXT_LEVEL_AVX2` is not ported yet; every
    /// encoder codes a codeblock with the same bytes).
    #[allow(clippy::too_many_arguments)]
    fn encode_cb32(
        buf: &[u32],
        missing_msbs: u32,
        num_passes: u32,
        width: u32,
        height: u32,
        stride: u32,
        lengths: &mut [u32; 2],
        coded: &mut Option<Vec<u8>>,
    ) -> OjphResult<()> {
        #[cfg(target_arch = "x86_64")]
        if crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX512
            && let Some(r) = crate::block_encoder_avx512::encode_codeblock_avx512(
                buf,
                missing_msbs,
                num_passes,
                width,
                height,
                stride,
                lengths,
                coded,
            )
        {
            return r;
        }
        ojph_encode_codeblock32(
            buf,
            missing_msbs,
            num_passes,
            width,
            height,
            stride,
            lengths,
            coded,
        )
    }

    /// `decode_cb32` as `codeblock_fun::init` chooses it
    /// (external/OpenJPH/src/core/codestream/ojph_codeblock_fun.cpp:215-227 @ OpenJPH 0.32.0):
    /// the AVX2 decoder at `X86_CPU_EXT_LEVEL_AVX2` and above, else the generic one (the SSSE3
    /// decoder upstream chooses from `X86_CPU_EXT_LEVEL_SSSE3` is not ported yet).
    #[allow(clippy::too_many_arguments)]
    fn decode_cb32(
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
    ) -> bool {
        #[cfg(target_arch = "x86_64")]
        if crate::arch::get_cpu_ext_level() >= crate::arch::x86_level::AVX2
            && let Some(r) = crate::block_decoder_avx2::decode_codeblock_avx2(
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
        {
            return r;
        }
        ojph_decode_codeblock32(
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
                Self::decode_cb32(
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
