// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_compress_literals.c` (external/zstd/lib/compress/zstd_compress_literals.c:1-235
//! @ zstd v1.5.7): a block's literals section, raw, RLE or Huffman-coded.

use crate::error_private::{ErrorCode, ZResult};
use crate::huf_compress::{
    HUF_FLAGS_BMI2, HUF_FLAGS_OPTIMAL_DEPTH, HUF_FLAGS_PREFER_REPEAT,
    HUF_FLAGS_SUSPECT_UNCOMPRESSIBLE, HUF_SYMBOLVALUE_MAX, HufCompressTables, HufRepeat,
    huf_compress1x_repeat, huf_compress4x_repeat,
};
use crate::zstd_compress_internal::{
    HufCTables, SET_BASIC, SET_COMPRESSED, SET_REPEAT, SET_RLE, Strategy, ZSTD_BTULTRA, ZSTD_LAZY,
    min_gain,
};

/// `LitHufLog` (external/zstd/lib/common/zstd_internal.h:101 @ zstd v1.5.7).
const LIT_HUF_LOG: u32 = 11;

/// `MEM_writeLE32` of a header into the first bytes of `dst`.
#[inline]
fn write_le32(dst: &mut [u8], v: u32) {
    dst[..4].copy_from_slice(&v.to_le_bytes());
}

/// Port of `ZSTD_noCompressLiterals` (external/zstd/lib/compress/zstd_compress_literals.c:39-66
/// @ zstd v1.5.7): the literals stored raw.
pub(crate) fn zstd_no_compress_literals(dst: &mut [u8], src: &[u8]) -> ZResult<usize> {
    let src_size = src.len();
    let fl_size = 1 + usize::from(src_size > 31) + usize::from(src_size > 4095);
    if src_size + fl_size > dst.len() {
        return Err(ErrorCode::DstSizeTooSmall);
    }
    match fl_size {
        1 => dst[0] = (SET_BASIC + ((src_size as u32) << 3)) as u8, // 2 - 1 - 5
        2 => {
            // 2 - 2 - 12
            let v = (SET_BASIC + (1 << 2) + ((src_size as u32) << 4)) as u16;
            dst[..2].copy_from_slice(&v.to_le_bytes());
        }
        _ => write_le32(dst, SET_BASIC + (3 << 2) + ((src_size as u32) << 4)), // 2 - 2 - 20
    }
    dst[fl_size..fl_size + src_size].copy_from_slice(src);
    Ok(src_size + fl_size)
}

/// Port of `allBytesIdentical` (external/zstd/lib/compress/zstd_compress_literals.c:68-79
/// @ zstd v1.5.7).
fn all_bytes_identical(src: &[u8]) -> bool {
    let b = src[0];
    src[1..].iter().all(|&p| p == b)
}

/// Port of `ZSTD_compressRleLiteralsBlock`
/// (external/zstd/lib/compress/zstd_compress_literals.c:81-107 @ zstd v1.5.7): one byte
/// repeated; `dst` has room for 4 bytes.
pub(crate) fn zstd_compress_rle_literals_block(dst: &mut [u8], src: &[u8]) -> usize {
    let src_size = src.len();
    let fl_size = 1 + usize::from(src_size > 31) + usize::from(src_size > 4095);
    match fl_size {
        1 => dst[0] = (SET_RLE + ((src_size as u32) << 3)) as u8, // 2 - 1 - 5
        2 => {
            // 2 - 2 - 12
            let v = (SET_RLE + (1 << 2) + ((src_size as u32) << 4)) as u16;
            dst[..2].copy_from_slice(&v.to_le_bytes());
        }
        _ => write_le32(dst, SET_RLE + (3 << 2) + ((src_size as u32) << 4)), // 2 - 2 - 20
    }
    dst[fl_size] = src[0];
    fl_size + 1
}

/// Port of `ZSTD_minLiteralsToCompress`
/// (external/zstd/lib/compress/zstd_compress_literals.c:115-127 @ zstd v1.5.7): the fewest
/// literals worth Huffman-coding, 8 for btultra2, twice as many per strategy below, at most 64.
fn zstd_min_literals_to_compress(strategy: Strategy, huf_repeat: HufRepeat) -> usize {
    let s = 9 - strategy as i32;
    let shift = if s < 3 { s } else { 3 };
    if huf_repeat == HufRepeat::Valid {
        6
    } else {
        8usize << shift
    }
}

/// Port of `ZSTD_compressLiterals` (external/zstd/lib/compress/zstd_compress_literals.c:129-235
/// @ zstd v1.5.7): `src`, the literals, into `dst`; `next_huf` gets the table the next
/// block may reuse.
#[allow(clippy::too_many_arguments)]
pub(crate) fn zstd_compress_literals(
    dst: &mut [u8],
    src: &[u8],
    entropy_workspace: &mut HufCompressTables,
    prev_huf: &HufCTables,
    next_huf: &mut HufCTables,
    strategy: Strategy,
    disable_literal_compression: bool,
    suspect_uncompressible: bool,
    bmi2: bool,
) -> ZResult<usize> {
    let src_size = src.len();
    let dst_capacity = dst.len();
    let lh_size = 3 + usize::from(src_size >= 1024) + usize::from(src_size >= 16 * 1024);
    let mut single_stream = src_size < 256;
    let mut h_type = SET_COMPRESSED;

    // Prepare nextEntropy assuming reusing the existing table
    *next_huf = *prev_huf;

    if disable_literal_compression {
        return zstd_no_compress_literals(dst, src);
    }

    // if too small, don't even attempt compression (speed opt)
    if src_size < zstd_min_literals_to_compress(strategy, prev_huf.repeat_mode) {
        return zstd_no_compress_literals(dst, src);
    }

    if dst_capacity < lh_size + 1 {
        return Err(ErrorCode::DstSizeTooSmall); // not enough space for compression
    }
    let c_lit_size = {
        let mut repeat = prev_huf.repeat_mode;
        let flags = (if bmi2 { HUF_FLAGS_BMI2 } else { 0 })
            | (if strategy < ZSTD_LAZY && src_size <= 1024 {
                HUF_FLAGS_PREFER_REPEAT
            } else {
                0
            })
            | (if strategy >= ZSTD_BTULTRA {
                HUF_FLAGS_OPTIMAL_DEPTH
            } else {
                0
            })
            | (if suspect_uncompressible {
                HUF_FLAGS_SUSPECT_UNCOMPRESSIBLE
            } else {
                0
            });

        if repeat == HufRepeat::Valid && lh_size == 3 {
            single_stream = true;
        }
        let huf_compress = if single_stream {
            huf_compress1x_repeat
        } else {
            huf_compress4x_repeat
        };
        let c_lit_size = huf_compress(
            &mut dst[lh_size..],
            src,
            HUF_SYMBOLVALUE_MAX,
            LIT_HUF_LOG,
            entropy_workspace,
            &mut next_huf.ctable,
            &mut repeat,
            flags,
        );
        if repeat != HufRepeat::None {
            // reused the existing table
            h_type = SET_REPEAT;
        }
        c_lit_size
    };

    {
        let min_gain = min_gain(src_size, strategy);
        match c_lit_size {
            Ok(c) if c != 0 && c < src_size - min_gain => {}
            _ => {
                *next_huf = *prev_huf;
                return zstd_no_compress_literals(dst, src);
            }
        }
    }
    let c_lit_size = c_lit_size.unwrap_or(0);
    if c_lit_size == 1 {
        // A return value of 1 signals that the alphabet consists of a single symbol. However,
        // in some rare circumstances, it could be the compressed size (a single byte). For
        // that outcome to have a chance to happen, it's necessary that `srcSize < 8`.
        if (src_size >= 8) || all_bytes_identical(src) {
            *next_huf = *prev_huf;
            return Ok(zstd_compress_rle_literals_block(dst, src));
        }
    }

    if h_type == SET_COMPRESSED {
        // using a newly constructed table
        next_huf.repeat_mode = HufRepeat::Check;
    }

    // Build header
    match lh_size {
        3 => {
            // 2 - 2 - 10 - 10
            let lhc = h_type
                + (u32::from(!single_stream) << 2)
                + ((src_size as u32) << 4)
                + ((c_lit_size as u32) << 14);
            dst[..3].copy_from_slice(&lhc.to_le_bytes()[..3]);
        }
        4 => {
            // 2 - 2 - 14 - 14
            let lhc = h_type + (2 << 2) + ((src_size as u32) << 4) + ((c_lit_size as u32) << 18);
            write_le32(dst, lhc);
        }
        _ => {
            // 2 - 2 - 18 - 18
            let lhc = h_type + (3 << 2) + ((src_size as u32) << 4) + ((c_lit_size as u32) << 22);
            write_le32(dst, lhc);
            dst[4] = (c_lit_size >> 10) as u8;
        }
    }
    Ok(lh_size + c_lit_size)
}
