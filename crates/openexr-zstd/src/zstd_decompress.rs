// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of the one-shot decompression of `zstd_decompress.c`
//! (external/zstd/lib/decompress/zstd_decompress.c:1-2410 @ zstd v1.5.7): frames, their
//! headers and blocks, skippable frames and checksums, without dictionaries (Core uses
//! none). The streaming API is not ported.

use crate::error_private::{ErrorCode, ZResult};
use crate::mem::{read_le16, read_le32, read_le64};
use crate::xxhash::xxh64;
use crate::zstd_decompress_block::{
    BlockProperties, BlockType, decompress_block_internal, get_c_block_size,
};
use crate::zstd_decompress_internal::{
    BLOCK_HEADER_SIZE, BLOCKSIZE_MAX, DCtx as Ctx, FrameHeader, REP_START_VALUE, TableSel,
};

/// `ZSTD_MAGICNUMBER` (external/zstd/lib/zstd.h:143 @ zstd v1.5.7).
const MAGICNUMBER: u32 = 0xFD2F_B528;
/// `ZSTD_MAGIC_SKIPPABLE_START` and `_MASK` (external/zstd/lib/zstd.h:145-146 @ zstd v1.5.7).
const MAGIC_SKIPPABLE_START: u32 = 0x184D_2A50;
const MAGIC_SKIPPABLE_MASK: u32 = 0xFFFF_FFF0;
/// `ZSTD_FRAMEIDSIZE` and `ZSTD_SKIPPABLEHEADERSIZE` (external/zstd/lib/zstd.h:1390-1394
/// @ zstd v1.5.7).
const FRAMEIDSIZE: usize = 4;
const SKIPPABLEHEADERSIZE: usize = 8;
/// `ZSTD_FRAMEHEADERSIZE_PREFIX` (and `ZSTD_startingInputLength`) and `_MIN` for
/// `ZSTD_f_zstd1` (external/zstd/lib/zstd.h:1386-1389 @ zstd v1.5.7).
const FRAMEHEADERSIZE_PREFIX: usize = 5;
const FRAMEHEADERSIZE_MIN: usize = 6;
/// `ZSTD_WINDOWLOG_ABSOLUTEMIN` and `ZSTD_WINDOWLOG_MAX` on 64-bit hosts
/// (external/zstd/lib/zstd.h:1251-1256 @ zstd v1.5.7).
const WINDOWLOG_ABSOLUTEMIN: u32 = 10;
const WINDOWLOG_MAX: u32 = 31;

/// `ZSTD_CONTENTSIZE_UNKNOWN` (external/zstd/lib/zstd.h:241 @ zstd v1.5.7).
pub const CONTENTSIZE_UNKNOWN: u64 = u64::MAX;
/// `ZSTD_CONTENTSIZE_ERROR` (external/zstd/lib/zstd.h:242 @ zstd v1.5.7).
pub const CONTENTSIZE_ERROR: u64 = u64::MAX - 1;

/// `ZSTD_did_fieldSize` and `ZSTD_fcs_fieldSize` (external/zstd/lib/common/zstd_internal.h:86-87
/// @ zstd v1.5.7).
const DID_FIELD_SIZE: [usize; 4] = [0, 1, 2, 4];
const FCS_FIELD_SIZE: [usize; 4] = [0, 2, 4, 8];

/// Port of `ZSTD_frameHeaderSize_internal` (external/zstd/lib/decompress/zstd_decompress.c:416-429
/// @ zstd v1.5.7).
fn frame_header_size(src: &[u8]) -> ZResult<usize> {
    let min_input_size = FRAMEHEADERSIZE_PREFIX;
    if src.len() < min_input_size {
        return Err(ErrorCode::SrcSizeWrong);
    }
    let fhd = src[min_input_size - 1];
    let dict_id = usize::from(fhd & 3);
    let single_segment = (fhd >> 5) & 1;
    let fcs_id = usize::from(fhd >> 6);
    Ok(min_input_size
        + usize::from(single_segment == 0)
        + DID_FIELD_SIZE[dict_id]
        + FCS_FIELD_SIZE[fcs_id]
        + usize::from(single_segment != 0 && fcs_id == 0))
}

/// Port of `ZSTD_getFrameHeader_advanced` (external/zstd/lib/decompress/zstd_decompress.c:447-551
/// @ zstd v1.5.7), for the zstd1 format: `Ok(0)` with `zfh` filled, `Ok(n)` when `n` bytes
/// are needed.
fn get_frame_header(zfh: &mut FrameHeader, src: &[u8]) -> ZResult<usize> {
    let src_size = src.len();
    let min_input_size = FRAMEHEADERSIZE_PREFIX;

    if src_size < min_input_size {
        if src_size > 0 {
            // when receiving less than @minInputSize bytes, control these bytes at least
            // correspond to a supported magic number in order to error out early if they
            // don't.
            let to_copy = src_size.min(4);
            let mut hbuf = MAGICNUMBER.to_le_bytes();
            hbuf[..to_copy].copy_from_slice(&src[..to_copy]);
            if u32::from_le_bytes(hbuf) != MAGICNUMBER {
                // not a zstd frame : let's check if it's a skippable frame
                let mut hbuf = MAGIC_SKIPPABLE_START.to_le_bytes();
                hbuf[..to_copy].copy_from_slice(&src[..to_copy]);
                if (u32::from_le_bytes(hbuf) & MAGIC_SKIPPABLE_MASK) != MAGIC_SKIPPABLE_START {
                    return Err(ErrorCode::PrefixUnknown);
                }
            }
        }
        return Ok(min_input_size);
    }

    *zfh = FrameHeader::default();
    if read_le32(src) != MAGICNUMBER {
        if (read_le32(src) & MAGIC_SKIPPABLE_MASK) == MAGIC_SKIPPABLE_START {
            // skippable frame
            if src_size < SKIPPABLEHEADERSIZE {
                return Ok(SKIPPABLEHEADERSIZE); // magic number + frame length
            }
            *zfh = FrameHeader::default();
            zfh.skippable = true;
            zfh.dict_id = read_le32(src).wrapping_sub(MAGIC_SKIPPABLE_START);
            zfh.header_size = SKIPPABLEHEADERSIZE as u32;
            zfh.frame_content_size = u64::from(read_le32(&src[FRAMEIDSIZE..]));
            return Ok(0);
        }
        return Err(ErrorCode::PrefixUnknown);
    }

    // ensure there is enough `srcSize` to fully read/decode frame header
    let fhsize = frame_header_size(src)?;
    if src_size < fhsize {
        return Ok(fhsize);
    }
    zfh.header_size = fhsize as u32;

    let fhd_byte = src[min_input_size - 1];
    let mut pos = min_input_size;
    let dict_id_size_code = fhd_byte & 3;
    let checksum_flag = u32::from((fhd_byte >> 2) & 1);
    let single_segment = (fhd_byte >> 5) & 1;
    let fcs_id = fhd_byte >> 6;
    let mut window_size: u64 = 0;
    let mut dict_id: u32 = 0;
    let mut frame_content_size = CONTENTSIZE_UNKNOWN;
    if (fhd_byte & 0x08) != 0 {
        return Err(ErrorCode::FrameParameterUnsupported); // reserved bits, must be zero
    }

    if single_segment == 0 {
        let wl_byte = src[pos];
        pos += 1;
        let window_log = u32::from(wl_byte >> 3) + WINDOWLOG_ABSOLUTEMIN;
        if window_log > WINDOWLOG_MAX {
            return Err(ErrorCode::FrameParameterWindowTooLarge);
        }
        window_size = 1u64 << window_log;
        window_size += (window_size >> 3) * u64::from(wl_byte & 7);
    }
    match dict_id_size_code {
        1 => {
            dict_id = u32::from(src[pos]);
            pos += 1;
        }
        2 => {
            dict_id = u32::from(read_le16(&src[pos..]));
            pos += 2;
        }
        3 => {
            dict_id = read_le32(&src[pos..]);
            pos += 4;
        }
        _ => {}
    }
    match fcs_id {
        1 => frame_content_size = u64::from(read_le16(&src[pos..])) + 256,
        2 => frame_content_size = u64::from(read_le32(&src[pos..])),
        3 => frame_content_size = read_le64(&src[pos..]),
        _ => {
            if single_segment != 0 {
                frame_content_size = u64::from(src[pos]);
            }
        }
    }
    if single_segment != 0 {
        window_size = frame_content_size;
    }

    zfh.skippable = false;
    zfh.frame_content_size = frame_content_size;
    zfh.window_size = window_size;
    zfh.block_size_max = window_size.min(BLOCKSIZE_MAX as u64) as u32;
    zfh.dict_id = dict_id;
    zfh.checksum_flag = checksum_flag;
    Ok(0)
}

/// Port of `ZSTD_getFrameContentSize` (external/zstd/lib/decompress/zstd_decompress.c:569-585
/// @ zstd v1.5.7): the decompressed size of the frame at the front of `src` if its header
/// gives it, [`CONTENTSIZE_UNKNOWN`] if not, [`CONTENTSIZE_ERROR`] for a bad header.
pub fn get_frame_content_size(src: &[u8]) -> u64 {
    let mut zfh = FrameHeader::default();
    if get_frame_header(&mut zfh, src) != Ok(0) {
        return CONTENTSIZE_ERROR;
    }
    if zfh.skippable {
        0
    } else {
        zfh.frame_content_size
    }
}

/// Port of `readSkippableFrameSize` (external/zstd/lib/decompress/zstd_decompress.c:587-601
/// @ zstd v1.5.7).
fn read_skippable_frame_size(src: &[u8]) -> ZResult<usize> {
    if src.len() < SKIPPABLEHEADERSIZE {
        return Err(ErrorCode::SrcSizeWrong);
    }
    let size_u32 = read_le32(&src[FRAMEIDSIZE..]);
    if size_u32.wrapping_add(SKIPPABLEHEADERSIZE as u32) < size_u32 {
        return Err(ErrorCode::FrameParameterUnsupported);
    }
    let skippable_size = SKIPPABLEHEADERSIZE + size_u32 as usize;
    if skippable_size > src.len() {
        return Err(ErrorCode::SrcSizeWrong);
    }
    Ok(skippable_size)
}

/// Port of `ZSTD_decodeFrameHeader` (external/zstd/lib/decompress/zstd_decompress.c:702-724
/// @ zstd v1.5.7), without a dictionary.
fn decode_frame_header(dctx: &mut Ctx, src: &[u8]) -> ZResult<()> {
    let result = get_frame_header(&mut dctx.f_params, src)?;
    if result > 0 {
        return Err(ErrorCode::SrcSizeWrong); // headerSize too small
    }
    if dctx.f_params.dict_id != 0 {
        // the context has no dictionary (dictID 0)
        return Err(ErrorCode::DictionaryWrong);
    }
    dctx.validate_checksum = dctx.f_params.checksum_flag != 0;
    Ok(())
}

/// Port of `ZSTD_decompressBegin` (external/zstd/lib/decompress/zstd_decompress.c:1560-1586
/// @ zstd v1.5.7).
fn decompress_begin(dctx: &mut Ctx) {
    // cover both little and big endian
    dctx.huf_table.table_type = 0;
    dctx.huf_table.table_log = 0;
    dctx.lit_entropy = false;
    dctx.fse_entropy = false;
    dctx.is_frame_decompression = true;
    dctx.rep = REP_START_VALUE; // initial repcodes
    dctx.ll_ptr = TableSel::Own;
    dctx.ml_ptr = TableSel::Own;
    dctx.of_ptr = TableSel::Own;
}

/// Port of `ZSTD_decompressFrame` (external/zstd/lib/decompress/zstd_decompress.c:953-1066
/// @ zstd v1.5.7): the frame at the front of `*src` into `dst[op_start..]`; advances `*src`
/// past it and returns the bytes written.
fn decompress_frame(
    dctx: &mut Ctx,
    dst: &mut [u8],
    op_start: usize,
    src: &mut &[u8],
) -> ZResult<usize> {
    let mut ip: &[u8] = src;
    let oend = dst.len();
    let mut op = op_start;

    // check
    if ip.len() < FRAMEHEADERSIZE_MIN + BLOCK_HEADER_SIZE {
        return Err(ErrorCode::SrcSizeWrong);
    }

    // Frame Header
    let frame_header_size = frame_header_size(&ip[..FRAMEHEADERSIZE_PREFIX])?;
    if ip.len() < frame_header_size + BLOCK_HEADER_SIZE {
        return Err(ErrorCode::SrcSizeWrong);
    }
    decode_frame_header(dctx, &ip[..frame_header_size])?;
    ip = &ip[frame_header_size..];

    // Loop on each block
    loop {
        let mut bp = BlockProperties {
            block_type: BlockType::Reserved,
            last_block: false,
            orig_size: 0,
        };
        let c_block_size = get_c_block_size(ip, &mut bp)?;
        ip = &ip[BLOCK_HEADER_SIZE..];
        if c_block_size > ip.len() {
            return Err(ErrorCode::SrcSizeWrong);
        }

        let decoded_size = match bp.block_type {
            BlockType::Compressed => {
                decompress_block_internal(dctx, dst, op, oend - op, &ip[..c_block_size])?
            }
            BlockType::Raw => {
                if c_block_size > oend - op {
                    return Err(ErrorCode::DstSizeTooSmall);
                }
                dst[op..op + c_block_size].copy_from_slice(&ip[..c_block_size]);
                c_block_size
            }
            BlockType::Rle => {
                let regen_size = bp.orig_size as usize;
                if regen_size > oend - op {
                    return Err(ErrorCode::DstSizeTooSmall);
                }
                dst[op..op + regen_size].fill(ip[0]);
                regen_size
            }
            BlockType::Reserved => return Err(ErrorCode::CorruptionDetected), // invalid block type
        };
        op += decoded_size;
        ip = &ip[c_block_size..];
        if bp.last_block {
            break;
        }
    }

    if dctx.f_params.frame_content_size != CONTENTSIZE_UNKNOWN
        && (op - op_start) as u64 != dctx.f_params.frame_content_size
    {
        return Err(ErrorCode::CorruptionDetected);
    }
    if dctx.f_params.checksum_flag != 0 {
        // Frame content checksum verification
        if ip.len() < 4 {
            return Err(ErrorCode::ChecksumWrong);
        }
        let check_calc = xxh64(&dst[op_start..op], 0) as u32;
        let check_read = read_le32(ip);
        if check_read != check_calc {
            return Err(ErrorCode::ChecksumWrong);
        }
        ip = &ip[4..];
    }

    // Allow caller to get size read
    *src = ip;
    Ok(op - op_start)
}

/// Port of `ZSTD_decompressMultiFrame` (external/zstd/lib/decompress/zstd_decompress.c:1068-1169
/// @ zstd v1.5.7), without a dictionary and with no legacy formats (OpenEXR builds without
/// `ZSTD_LEGACY_SUPPORT`).
fn decompress_multi_frame(dctx: &mut Ctx, dst: &mut [u8], mut src: &[u8]) -> ZResult<usize> {
    let mut op = 0usize;
    let mut more_than_1_frame = false;

    while src.len() >= FRAMEHEADERSIZE_PREFIX {
        if src.len() >= 4 {
            let magic_number = read_le32(src);
            if (magic_number & MAGIC_SKIPPABLE_MASK) == MAGIC_SKIPPABLE_START {
                // skippable frame detected : skip it
                let skippable_size = read_skippable_frame_size(src)?;
                src = &src[skippable_size..];
                continue; // check next frame
            }
        }

        decompress_begin(dctx);
        // ZSTD_checkContinuity: the frame's output starts a new prefix
        dctx.prefix_start = op;
        match decompress_frame(dctx, dst, op, &mut src) {
            Err(ErrorCode::PrefixUnknown) if more_than_1_frame => {
                // At least one frame successfully completed, but following bytes are
                // garbage: it's more likely to be a srcSize error.
                return Err(ErrorCode::SrcSizeWrong);
            }
            Err(e) => return Err(e),
            Ok(res) => op += res,
        }
        more_than_1_frame = true;
    }

    if !src.is_empty() {
        return Err(ErrorCode::SrcSizeWrong); // input not entirely consumed
    }
    Ok(op)
}

/// A decompression context (`ZSTD_DCtx`), which keeps its tables between calls.
#[derive(Debug, Clone)]
pub struct DCtx(Ctx);

impl Default for DCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl DCtx {
    /// Port of `ZSTD_createDCtx` (external/zstd/lib/decompress/zstd_decompress.c:310-314
    /// @ zstd v1.5.7).
    pub fn new() -> Self {
        DCtx(Ctx::new())
    }

    /// Port of `ZSTD_DCtx_reset (dctx, ZSTD_reset_session_only)`
    /// (external/zstd/lib/decompress/zstd_decompress.c:1947-1962 @ zstd v1.5.7): only the
    /// streaming session, which one-shot decompression does not use.
    pub fn reset_session_only(&mut self) {}

    /// Port of `ZSTD_decompressDCtx` (external/zstd/lib/decompress/zstd_decompress.c:1197-1200
    /// @ zstd v1.5.7): every frame of `src` into `dst`; returns the bytes written.
    pub fn decompress_dctx(&mut self, dst: &mut [u8], src: &[u8]) -> ZResult<usize> {
        decompress_multi_frame(&mut self.0, dst, src)
    }
}
