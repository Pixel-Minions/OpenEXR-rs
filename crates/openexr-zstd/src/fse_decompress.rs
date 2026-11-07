// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `fse_decompress.c` (external/zstd/lib/common/fse_decompress.c:1-315 @ zstd v1.5.7)
//! and the decoding half of `fse.h` (external/zstd/lib/common/fse.h:500-560 @ zstd v1.5.7):
//! FSE decoding tables and the byte decoder zstd uses for Huffman weights.
//!
//! Upstream builds its tables in a caller's workspace and checks the workspace's size; the
//! port allocates its tables and keeps the checks, with the sizes upstream computes, since
//! they decide which headers are refused.

use crate::bitstream::{BitDStream, BitDStreamStatus};
use crate::entropy_common::fse_read_ncount;
use crate::error_private::{ErrorCode, ZResult};
use crate::mem::highbit32;

/// `FSE_MAX_SYMBOL_VALUE` (external/zstd/lib/common/fse.h:595 @ zstd v1.5.7).
pub(crate) const FSE_MAX_SYMBOL_VALUE: u32 = 255;
/// `FSE_MAX_TABLELOG` (external/zstd/lib/common/fse.h:612 @ zstd v1.5.7):
/// `FSE_MAX_MEMORY_USAGE - 2`.
pub(crate) const FSE_MAX_TABLELOG: u32 = 14 - 2;

/// `FSE_DTABLE_SIZE_U32 (maxTableLog)`.
const fn dtable_size_u32(max_table_log: u32) -> usize {
    1 + (1 << max_table_log)
}

/// `FSE_BUILD_DTABLE_WKSP_SIZE (maxTableLog, maxSymbolValue)`, in bytes.
const fn build_dtable_wksp_size(max_table_log: u32, max_symbol_value: u32) -> usize {
    2 * (max_symbol_value as usize + 1) + (1 << max_table_log) + 8
}

/// `FSE_DECOMPRESS_WKSP_SIZE (maxTableLog, maxSymbolValue)`, in bytes
/// (external/zstd/lib/common/fse.h:272-273 @ zstd v1.5.7).
pub(crate) const fn fse_decompress_wksp_size(max_table_log: u32, max_symbol_value: u32) -> usize {
    let build_u32 = build_dtable_wksp_size(max_table_log, max_symbol_value).div_ceil(4);
    (dtable_size_u32(max_table_log) + 1 + build_u32 + (FSE_MAX_SYMBOL_VALUE as usize + 1) / 2 + 1)
        * 4
}

/// Port of `FSE_decode_t` (external/zstd/lib/common/fse.h:510-515 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FseDecode {
    pub(crate) new_state: u16,
    pub(crate) symbol: u8,
    pub(crate) nb_bits: u8,
}

/// An `FSE_DTable`: its header (`FSE_DTableHeader`) and its cells.
#[derive(Debug, Clone)]
pub(crate) struct FseDTable {
    pub(crate) table_log: u16,
    pub(crate) fast_mode: u16,
    pub(crate) decode: Vec<FseDecode>,
}

/// `FSE_TABLESTEP` (external/zstd/lib/common/fse.h:623 @ zstd v1.5.7).
const fn table_step(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

/// Port of `FSE_buildDTable_internal` (external/zstd/lib/common/fse_decompress.c:58-159
/// @ zstd v1.5.7): the decoding table of `normalized_counter`; `wksp_size` is the workspace
/// upstream has left for it.
fn fse_build_dtable(
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
    wksp_size: usize,
) -> ZResult<FseDTable> {
    let max_sv1 = max_symbol_value + 1;
    let table_size = 1u32 << table_log;
    let mut high_threshold = table_size - 1;

    // Sanity Checks
    if build_dtable_wksp_size(table_log, max_symbol_value) > wksp_size {
        return Err(ErrorCode::MaxSymbolValueTooLarge);
    }
    if max_symbol_value > FSE_MAX_SYMBOL_VALUE {
        return Err(ErrorCode::MaxSymbolValueTooLarge);
    }
    if table_log > FSE_MAX_TABLELOG {
        return Err(ErrorCode::TableLogTooLarge);
    }

    let mut decode = vec![FseDecode::default(); table_size as usize];
    let mut symbol_next = vec![0u16; max_sv1 as usize];
    let mut fast_mode: u16 = 1;

    // Init, lay down lowprob symbols
    let large_limit = (1i32 << (table_log - 1)) as i16;
    for s in 0..max_sv1 as usize {
        if normalized_counter[s] == -1 {
            decode[high_threshold as usize].symbol = s as u8;
            high_threshold = high_threshold.wrapping_sub(1);
            symbol_next[s] = 1;
        } else {
            if normalized_counter[s] >= large_limit {
                fast_mode = 0;
            }
            symbol_next[s] = normalized_counter[s] as u16;
        }
    }

    // Spread symbols
    let table_mask = table_size - 1;
    let step = table_step(table_size);
    if high_threshold == table_size - 1 {
        // First lay down the symbols in order, then spread those positions across the
        // table: the k-th position goes to (k * step) & mask, as the one-stage loop below.
        let mut spread = Vec::with_capacity(table_size as usize);
        for s in 0..max_sv1 as usize {
            for _ in 0..normalized_counter[s].max(0) {
                spread.push(s as u8);
            }
        }
        let mut position: u32 = 0;
        for &sym in spread.iter().take(table_size as usize) {
            decode[position as usize].symbol = sym;
            position = (position + step) & table_mask;
        }
    } else {
        let mut position: u32 = 0;
        for s in 0..max_sv1 as usize {
            for _ in 0..normalized_counter[s] {
                decode[position as usize].symbol = s as u8;
                position = (position + step) & table_mask;
                while position > high_threshold {
                    position = (position + step) & table_mask; // lowprob area
                }
            }
        }
        if position != 0 {
            // position must reach all cells once, otherwise normalizedCounter is incorrect
            return Err(ErrorCode::Generic);
        }
    }

    // Build Decoding table
    for cell in decode.iter_mut() {
        let symbol = usize::from(cell.symbol);
        let next_state = u32::from(symbol_next[symbol]);
        symbol_next[symbol] = symbol_next[symbol].wrapping_add(1);
        cell.nb_bits = (table_log - highbit32(next_state)) as u8;
        cell.new_state = (next_state << cell.nb_bits).wrapping_sub(table_size) as u16;
    }

    Ok(FseDTable {
        table_log: table_log as u16,
        fast_mode,
        decode,
    })
}

/// Port of `FSE_DState_t` with `FSE_initDState`, `FSE_decodeSymbol` and
/// `FSE_decodeSymbolFast` (external/zstd/lib/common/fse.h:517-560 @ zstd v1.5.7).
struct FseDState<'t> {
    state: usize,
    table: &'t [FseDecode],
}

impl<'t> FseDState<'t> {
    fn init(bit_d: &mut BitDStream<'_>, dt: &'t FseDTable) -> Self {
        let state = bit_d.read_bits(u32::from(dt.table_log)) as usize;
        bit_d.reload();
        FseDState {
            state,
            table: &dt.decode,
        }
    }

    /// `FSE_decodeSymbol`, or `FSE_decodeSymbolFast` when `fast` (no symbol has a probability
    /// above one half, so every cell reads at least one bit).
    fn decode_symbol(&mut self, bit_d: &mut BitDStream<'_>, fast: bool) -> u8 {
        let d_info = self.table[self.state];
        let nb_bits = u32::from(d_info.nb_bits);
        let low_bits = if fast {
            bit_d.read_bits_fast(nb_bits)
        } else {
            bit_d.read_bits(nb_bits)
        };
        self.state = (usize::from(d_info.new_state)).wrapping_add(low_bits as usize);
        d_info.symbol
    }
}

/// Port of `FSE_decompress_usingDTable_generic`
/// (external/zstd/lib/common/fse_decompress.c:173-236 @ zstd v1.5.7): two interleaved states
/// decode `c_src` into `dst`; returns the bytes decoded.
fn fse_decompress_using_dtable(
    dst: &mut [u8],
    c_src: &[u8],
    dt: &FseDTable,
    fast: bool,
) -> ZResult<usize> {
    let omax = dst.len() as isize;
    let olimit = omax - 3;
    let mut op: isize = 0;

    // Init
    let (mut bit_d, _) = BitDStream::init(c_src)?;
    let mut state1 = FseDState::init(&mut bit_d, dt);
    let mut state2 = FseDState::init(&mut bit_d, dt);

    if bit_d.reload() == BitDStreamStatus::Overflow {
        return Err(ErrorCode::CorruptionDetected);
    }

    // 4 symbols per loop (both conditions are evaluated, as upstream's `&`)
    loop {
        let unfinished = bit_d.reload() == BitDStreamStatus::Unfinished;
        if !(unfinished & (op < olimit)) {
            break;
        }
        let o = op as usize;
        dst[o] = state1.decode_symbol(&mut bit_d, fast);
        dst[o + 1] = state2.decode_symbol(&mut bit_d, fast);
        dst[o + 2] = state1.decode_symbol(&mut bit_d, fast);
        dst[o + 3] = state2.decode_symbol(&mut bit_d, fast);
        op += 4;
    }

    // tail
    // note : BIT_reloadDStream(&bitD) >= FSE_DStream_partiallyFilled; Ends at exactly
    // BIT_DStream_completed
    loop {
        if op > omax - 2 {
            return Err(ErrorCode::DstSizeTooSmall);
        }
        dst[op as usize] = state1.decode_symbol(&mut bit_d, fast);
        op += 1;
        if bit_d.reload() == BitDStreamStatus::Overflow {
            dst[op as usize] = state2.decode_symbol(&mut bit_d, fast);
            op += 1;
            break;
        }

        if op > omax - 2 {
            return Err(ErrorCode::DstSizeTooSmall);
        }
        dst[op as usize] = state2.decode_symbol(&mut bit_d, fast);
        op += 1;
        if bit_d.reload() == BitDStreamStatus::Overflow {
            dst[op as usize] = state1.decode_symbol(&mut bit_d, fast);
            op += 1;
            break;
        }
    }

    Ok(op as usize)
}

/// Port of `FSE_decompress_wksp_body` and `FSE_decompress_wksp_bmi2`
/// (external/zstd/lib/common/fse_decompress.c:243-313 @ zstd v1.5.7): an FSE header and
/// stream decoded into `dst`, with a table log of at most `max_log` and a workspace of
/// `wksp_size` bytes; returns the bytes decoded.
pub(crate) fn fse_decompress_wksp(
    dst: &mut [u8],
    c_src: &[u8],
    max_log: u32,
    wksp_size: usize,
) -> ZResult<usize> {
    let mut table_log = 0u32;
    let mut max_symbol_value = FSE_MAX_SYMBOL_VALUE;
    // sizeof (FSE_DecompressWksp)
    let ncount_bytes = 2 * (FSE_MAX_SYMBOL_VALUE as usize + 1);
    let mut ncount = [0i16; FSE_MAX_SYMBOL_VALUE as usize + 1];

    if wksp_size < ncount_bytes {
        return Err(ErrorCode::Generic);
    }

    // normal FSE decoding mode
    let ncount_length = fse_read_ncount(&mut ncount, &mut max_symbol_value, &mut table_log, c_src)?;
    if table_log > max_log {
        return Err(ErrorCode::TableLogTooLarge);
    }
    let ip = &c_src[ncount_length..];

    if fse_decompress_wksp_size(table_log, max_symbol_value) > wksp_size {
        return Err(ErrorCode::TableLogTooLarge);
    }
    let left = wksp_size - ncount_bytes - 4 * dtable_size_u32(table_log);

    let dtable = fse_build_dtable(&ncount, max_symbol_value, table_log, left)?;

    // select fast mode (static)
    fse_decompress_using_dtable(dst, ip, &dtable, dtable.fast_mode != 0)
}
