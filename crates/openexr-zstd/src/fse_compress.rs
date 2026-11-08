// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `fse_compress.c` (external/zstd/lib/compress/fse_compress.c:1-625 @ zstd v1.5.7)
//! and the encoding half of `fse.h` (external/zstd/lib/common/fse.h:420-500 @ zstd v1.5.7):
//! FSE normalization, headers, encoding tables and the byte encoder.
//!
//! An `FSE_CTable` is kept in upstream's layout, `u32` words: a header (`u16` table log and
//! largest symbol), the state table (`u16`s), then one transform (two words) per symbol.
//! zstd keeps these tables between blocks and reads cells a new table did not write (the
//! cost estimates of repeat mode), so the layout and the cells left in place are upstream's.

use crate::bitstream::BitCStream;
use crate::entropy_common::FSE_MIN_TABLELOG;
use crate::error_private::{ErrorCode, ZResult};
use crate::fse_decompress::FSE_MAX_TABLELOG;
use crate::mem::highbit32;

/// `FSE_DEFAULT_TABLELOG` (external/zstd/lib/common/fse.h:615 @ zstd v1.5.7):
/// `FSE_DEFAULT_MEMORY_USAGE - 2`.
pub(crate) const FSE_DEFAULT_TABLELOG: u32 = 13 - 2;
/// `FSE_NCOUNTBOUND` (external/zstd/lib/common/fse.h:235 @ zstd v1.5.7).
pub(crate) const FSE_NCOUNTBOUND: usize = 512;

/// `FSE_CTABLE_SIZE_U32 (maxTableLog, maxSymbolValue)` (external/zstd/lib/common/fse.h:240
/// @ zstd v1.5.7).
pub(crate) const fn fse_ctable_size_u32(max_table_log: u32, max_symbol_value: u32) -> usize {
    1 + (1 << (max_table_log - 1)) + ((max_symbol_value as usize + 1) * 2)
}

/// `FSE_BUILD_CTABLE_WORKSPACE_SIZE (maxSymbolValue, tableLog)`, in bytes
/// (external/zstd/lib/common/fse.h:263-264 @ zstd v1.5.7).
pub(crate) const fn fse_build_ctable_workspace_size(
    max_symbol_value: u32,
    table_log: u32,
) -> usize {
    4 * (((max_symbol_value as usize + 2) + (1usize << table_log)) / 2 + 2)
}

/// `FSE_BLOCKBOUND (size)` (external/zstd/lib/common/fse.h:236 @ zstd v1.5.7).
pub(crate) const fn fse_block_bound(size: usize) -> usize {
    size + (size >> 7) + 4 + 8
}

/// An `FSE_CTable` in upstream's word layout.
#[derive(Debug, Clone)]
pub(crate) struct FseCTable(pub(crate) Vec<u32>);

impl FseCTable {
    /// A table of `FSE_CTABLE_SIZE_U32 (max_table_log, max_symbol_value)` zeroed words.
    pub(crate) fn new(max_table_log: u32, max_symbol_value: u32) -> Self {
        FseCTable(vec![
            0;
            fse_ctable_size_u32(max_table_log, max_symbol_value)
        ])
    }

    #[inline]
    fn u16_at(&self, k: usize) -> u16 {
        (self.0[k / 2] >> (16 * (k % 2))) as u16
    }

    #[inline]
    fn set_u16(&mut self, k: usize, v: u16) {
        let w = &mut self.0[k / 2];
        let sh = 16 * (k % 2);
        *w = (*w & !(0xFFFF << sh)) | (u32::from(v) << sh);
    }

    /// The table log in the header (`MEM_read16 (ct)`).
    #[inline]
    pub(crate) fn table_log(&self) -> u32 {
        u32::from(self.u16_at(0))
    }

    /// The largest symbol in the header (`ZSTD_getFSEMaxSymbolValue`).
    #[inline]
    pub(crate) fn max_symbol_value(&self) -> u32 {
        u32::from(self.u16_at(1))
    }

    /// The word where the symbol transforms start.
    #[inline]
    fn symbol_tt_base(table_log: u32) -> usize {
        1 + if table_log != 0 {
            1 << (table_log - 1)
        } else {
            1
        }
    }

    /// `symbolTT[s].deltaFindState` and `.deltaNbBits`.
    #[inline]
    pub(crate) fn symbol_tt(&self, s: usize) -> (i32, u32) {
        let at = Self::symbol_tt_base(self.table_log()) + 2 * s;
        (self.0[at] as i32, self.0[at + 1])
    }

    #[inline]
    fn set_delta_nb_bits(&mut self, table_log: u32, s: usize, v: u32) {
        let at = Self::symbol_tt_base(table_log) + 2 * s;
        self.0[at + 1] = v;
    }

    #[inline]
    fn set_delta_find_state(&mut self, table_log: u32, s: usize, v: i32) {
        let at = Self::symbol_tt_base(table_log) + 2 * s;
        self.0[at] = v as u32;
    }

    /// `stateTable[i]` (`u16`s after the header).
    #[inline]
    fn state(&self, i: usize) -> u16 {
        self.u16_at(2 + i)
    }
}

/// `FSE_TABLESTEP`.
const fn table_step(table_size: u32) -> u32 {
    (table_size >> 1) + (table_size >> 3) + 3
}

/// Port of `FSE_buildCTable_wksp` (external/zstd/lib/compress/fse_compress.c:66-201
/// @ zstd v1.5.7): the encoding table of `normalized_counter`; `wksp_size` is the
/// workspace upstream has for it.
pub(crate) fn fse_build_ctable(
    ct: &mut FseCTable,
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
    wksp_size: usize,
) -> ZResult<()> {
    let table_size = 1u32 << table_log;
    let table_mask = table_size - 1;
    let step = table_step(table_size);
    let max_sv1 = max_symbol_value + 1;
    let mut cumul = vec![0u16; max_sv1 as usize + 1];
    let mut table_symbol = vec![0u8; table_size as usize];
    let mut high_threshold = table_size - 1;

    if fse_build_ctable_workspace_size(max_symbol_value, table_log) > wksp_size {
        return Err(ErrorCode::TableLogTooLarge);
    }

    // CTable header
    ct.set_u16(0, table_log as u16);
    ct.set_u16(1, max_symbol_value as u16);

    // symbol start positions
    cumul[0] = 0;
    for u in 1..=max_sv1 as usize {
        if normalized_counter[u - 1] == -1 {
            // Low proba symbol
            cumul[u] = cumul[u - 1] + 1;
            table_symbol[high_threshold as usize] = (u - 1) as u8;
            high_threshold = high_threshold.wrapping_sub(1);
        } else {
            cumul[u] = cumul[u - 1].wrapping_add(normalized_counter[u - 1] as u16);
        }
    }
    cumul[max_sv1 as usize] = (table_size + 1) as u16;

    // Spread symbols (upstream's two-stage spread without low-probability symbols puts the
    // k-th symbol at (k * step) & mask, as this loop does)
    let mut position: u32 = 0;
    for symbol in 0..max_sv1 as usize {
        for _ in 0..normalized_counter[symbol].max(0) {
            table_symbol[position as usize] = symbol as u8;
            position = (position + step) & table_mask;
            while position > high_threshold {
                position = (position + step) & table_mask; // Low proba area
            }
        }
    }

    // Build table
    for u in 0..table_size {
        let s = usize::from(table_symbol[u as usize]);
        let at = usize::from(cumul[s]);
        cumul[s] = cumul[s].wrapping_add(1);
        // TableU16 : sorted by symbol order; gives next state value
        ct.set_u16(2 + at, (table_size + u) as u16);
    }

    // Build Symbol Transformation Table
    let mut total: u32 = 0;
    for s in 0..=max_symbol_value as usize {
        match normalized_counter[s] {
            0 => {
                // filling nonetheless, for compatibility with FSE_getMaxNbBits()
                ct.set_delta_nb_bits(
                    table_log,
                    s,
                    ((table_log + 1) << 16).wrapping_sub(1 << table_log),
                );
            }
            -1 | 1 => {
                ct.set_delta_nb_bits(table_log, s, (table_log << 16).wrapping_sub(1 << table_log));
                ct.set_delta_find_state(table_log, s, total.wrapping_sub(1) as i32);
                total += 1;
            }
            n => {
                let max_bits_out = table_log - highbit32(n as u32 - 1);
                let min_state_plus = (n as u32) << max_bits_out;
                ct.set_delta_nb_bits(
                    table_log,
                    s,
                    (max_bits_out << 16).wrapping_sub(min_state_plus),
                );
                ct.set_delta_find_state(table_log, s, total.wrapping_sub(n as u32) as i32);
                total += n as u32;
            }
        }
    }
    Ok(())
}

/// Port of `FSE_NCountWriteBound` (external/zstd/lib/compress/fse_compress.c:209-217
/// @ zstd v1.5.7).
pub(crate) fn fse_ncount_write_bound(max_symbol_value: u32, table_log: u32) -> usize {
    let max_header_size =
        (((max_symbol_value as usize + 1) * table_log as usize + 4 + 2) / 8) + 1 + 2;
    if max_symbol_value != 0 {
        max_header_size
    } else {
        FSE_NCOUNTBOUND
    }
}

/// Port of `FSE_writeNCount_generic` (external/zstd/lib/compress/fse_compress.c:219-326
/// @ zstd v1.5.7).
fn fse_write_ncount_generic(
    header: &mut [u8],
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
    write_is_safe: bool,
) -> ZResult<usize> {
    let oend = header.len() as isize;
    let mut out: isize = 0;
    let table_size: i32 = 1 << table_log;
    let mut bit_stream: u32 = 0;
    let mut bit_count: i32 = 0;
    let mut symbol: u32 = 0;
    let alphabet_size = max_symbol_value + 1;
    let mut previous_is0 = false;

    let mut put2 = |out: &mut isize, bit_stream: u32| -> ZResult<()> {
        if !write_is_safe && *out > oend - 2 {
            return Err(ErrorCode::DstSizeTooSmall); // Buffer overflow
        }
        header[*out as usize] = bit_stream as u8;
        header[*out as usize + 1] = (bit_stream >> 8) as u8;
        *out += 2;
        Ok(())
    };

    // Table Size
    bit_stream = bit_stream.wrapping_add((table_log - FSE_MIN_TABLELOG) << bit_count);
    bit_count += 4;

    // Init
    let mut remaining: i32 = table_size + 1; // +1 for extra accuracy
    let mut threshold: i32 = table_size;
    let mut nb_bits: i32 = table_log as i32 + 1;

    while symbol < alphabet_size && remaining > 1 {
        // stops at 1
        if previous_is0 {
            let mut start = symbol;
            while symbol < alphabet_size && normalized_counter[symbol as usize] == 0 {
                symbol += 1;
            }
            if symbol == alphabet_size {
                break; // incorrect distribution
            }
            while symbol >= start + 24 {
                start += 24;
                bit_stream = bit_stream.wrapping_add(0xFFFF << bit_count);
                put2(&mut out, bit_stream)?;
                bit_stream >>= 16;
            }
            while symbol >= start + 3 {
                start += 3;
                bit_stream = bit_stream.wrapping_add(3 << bit_count);
                bit_count += 2;
            }
            bit_stream = bit_stream.wrapping_add((symbol - start) << bit_count);
            bit_count += 2;
            if bit_count > 16 {
                put2(&mut out, bit_stream)?;
                bit_stream >>= 16;
                bit_count -= 16;
            }
        }
        {
            let mut count = i32::from(normalized_counter[symbol as usize]);
            symbol += 1;
            let max = (2 * threshold - 1) - remaining;
            remaining -= if count < 0 { -count } else { count };
            count += 1; // +1 for extra accuracy
            if count >= threshold {
                count += max; // [0..max[ [max..threshold[ (...) [threshold+max 2*threshold[
            }
            bit_stream = bit_stream.wrapping_add((count as u32) << bit_count);
            bit_count += nb_bits;
            bit_count -= i32::from(count < max);
            previous_is0 = count == 1;
            if remaining < 1 {
                return Err(ErrorCode::Generic);
            }
            while remaining < threshold {
                nb_bits -= 1;
                threshold >>= 1;
            }
        }
        if bit_count > 16 {
            put2(&mut out, bit_stream)?;
            bit_stream >>= 16;
            bit_count -= 16;
        }
    }

    if remaining != 1 {
        return Err(ErrorCode::Generic); // incorrect normalized distribution
    }

    // flush remaining bitStream
    if !write_is_safe && out > oend - 2 {
        return Err(ErrorCode::DstSizeTooSmall); // Buffer overflow
    }
    header[out as usize] = bit_stream as u8;
    header[out as usize + 1] = (bit_stream >> 8) as u8;
    out += ((bit_count + 7) / 8) as isize;

    Ok(out as usize)
}

/// Port of `FSE_writeNCount` (external/zstd/lib/compress/fse_compress.c:328-337 @ zstd v1.5.7).
pub(crate) fn fse_write_ncount(
    buffer: &mut [u8],
    normalized_counter: &[i16],
    max_symbol_value: u32,
    table_log: u32,
) -> ZResult<usize> {
    if table_log > FSE_MAX_TABLELOG {
        return Err(ErrorCode::TableLogTooLarge); // Unsupported
    }
    if table_log < FSE_MIN_TABLELOG {
        return Err(ErrorCode::Generic); // Unsupported
    }
    let safe = buffer.len() >= fse_ncount_write_bound(max_symbol_value, table_log);
    fse_write_ncount_generic(
        buffer,
        normalized_counter,
        max_symbol_value,
        table_log,
        safe,
    )
}

/// Port of `FSE_minTableLog` (external/zstd/lib/compress/fse_compress.c:345-352 @ zstd v1.5.7).
fn fse_min_table_log(src_size: usize, max_symbol_value: u32) -> u32 {
    let min_bits_src = highbit32(src_size as u32) + 1;
    let min_bits_symbols = highbit32(max_symbol_value) + 2;
    if min_bits_src < min_bits_symbols {
        min_bits_src
    } else {
        min_bits_symbols
    }
}

/// Port of `FSE_optimalTableLog_internal` (external/zstd/lib/compress/fse_compress.c:354-367
/// @ zstd v1.5.7).
pub(crate) fn fse_optimal_table_log_internal(
    max_table_log: u32,
    src_size: usize,
    max_symbol_value: u32,
    minus: u32,
) -> u32 {
    let max_bits_src = highbit32((src_size - 1) as u32).wrapping_sub(minus);
    let mut table_log = max_table_log;
    let min_bits = fse_min_table_log(src_size, max_symbol_value);
    if table_log == 0 {
        table_log = FSE_DEFAULT_TABLELOG;
    }
    if max_bits_src < table_log {
        table_log = max_bits_src; // Accuracy can be reduced
    }
    if min_bits > table_log {
        table_log = min_bits; // Need a minimum to safely represent all symbol values
    }
    if table_log < FSE_MIN_TABLELOG {
        table_log = FSE_MIN_TABLELOG;
    }
    if table_log > FSE_MAX_TABLELOG {
        table_log = FSE_MAX_TABLELOG;
    }
    table_log
}

/// Port of `FSE_optimalTableLog` (external/zstd/lib/compress/fse_compress.c:369-372
/// @ zstd v1.5.7).
pub(crate) fn fse_optimal_table_log(
    max_table_log: u32,
    src_size: usize,
    max_symbol_value: u32,
) -> u32 {
    fse_optimal_table_log_internal(max_table_log, src_size, max_symbol_value, 2)
}

/// Port of `FSE_normalizeM2` (external/zstd/lib/compress/fse_compress.c:376-461 @ zstd v1.5.7):
/// the secondary normalization, when the primary one fails.
fn fse_normalize_m2(
    norm: &mut [i16],
    table_log: u32,
    count: &[u32],
    mut total: usize,
    max_symbol_value: u32,
    low_prob_count: i16,
) -> ZResult<()> {
    const NOT_YET_ASSIGNED: i16 = -2;
    let mut distributed: u32 = 0;

    // Init
    let low_threshold = (total >> table_log) as u32;
    let mut low_one = ((total * 3) >> (table_log + 1)) as u32;

    for s in 0..=max_symbol_value as usize {
        if count[s] == 0 {
            norm[s] = 0;
            continue;
        }
        if count[s] <= low_threshold {
            norm[s] = low_prob_count;
            distributed += 1;
            total -= count[s] as usize;
            continue;
        }
        if count[s] <= low_one {
            norm[s] = 1;
            distributed += 1;
            total -= count[s] as usize;
            continue;
        }
        norm[s] = NOT_YET_ASSIGNED;
    }
    let mut to_distribute = (1u32 << table_log) - distributed;

    if to_distribute == 0 {
        return Ok(());
    }

    if (total / to_distribute as usize) as u32 > low_one {
        // risk of rounding to zero
        low_one = ((total * 3) / (to_distribute as usize * 2)) as u32;
        for s in 0..=max_symbol_value as usize {
            if norm[s] == NOT_YET_ASSIGNED && count[s] <= low_one {
                norm[s] = 1;
                distributed += 1;
                total -= count[s] as usize;
                continue;
            }
        }
        to_distribute = (1u32 << table_log) - distributed;
    }

    if distributed == max_symbol_value + 1 {
        // all values are pretty poor; probably incompressible data (should have already been
        // detected); find max, then give all remaining points to max
        let (mut max_v, mut max_c) = (0usize, 0u32);
        for s in 0..=max_symbol_value as usize {
            if count[s] > max_c {
                max_v = s;
                max_c = count[s];
            }
        }
        norm[max_v] += to_distribute as i16;
        return Ok(());
    }

    if total == 0 {
        // all of the symbols were low enough for the lowOne or lowThreshold
        let mut s = 0usize;
        while to_distribute > 0 {
            if norm[s] > 0 {
                to_distribute -= 1;
                norm[s] += 1;
            }
            s = (s + 1) % (max_symbol_value as usize + 1);
        }
        return Ok(());
    }

    let v_step_log: u64 = 62 - u64::from(table_log);
    let mid: u64 = (1u64 << (v_step_log - 1)) - 1;
    let r_step: u64 =
        (((1u64 << v_step_log) * u64::from(to_distribute)) + mid) / (total as u32 as u64); // scale on remaining
    let mut tmp_total: u64 = mid;
    for s in 0..=max_symbol_value as usize {
        if norm[s] == NOT_YET_ASSIGNED {
            let end = tmp_total + (u64::from(count[s]) * r_step);
            let s_start = (tmp_total >> v_step_log) as u32;
            let s_end = (end >> v_step_log) as u32;
            let weight = s_end - s_start;
            if weight < 1 {
                return Err(ErrorCode::Generic);
            }
            norm[s] = weight as i16;
            tmp_total = end;
        }
    }
    Ok(())
}

/// Port of `FSE_normalizeCount` (external/zstd/lib/compress/fse_compress.c:463-521
/// @ zstd v1.5.7): `count` scaled to a total of `1 << table_log`; a symbol below
/// `1 / table size` gets -1 when `use_low_prob_count`, else 1. Returns the table log.
pub(crate) fn fse_normalize_count(
    normalized_counter: &mut [i16],
    table_log: u32,
    count: &[u32],
    total: usize,
    max_symbol_value: u32,
    use_low_prob_count: bool,
) -> ZResult<u32> {
    let mut table_log = table_log;
    // Sanity checks
    if table_log == 0 {
        table_log = FSE_DEFAULT_TABLELOG;
    }
    if table_log < FSE_MIN_TABLELOG {
        return Err(ErrorCode::Generic); // Unsupported size
    }
    if table_log > FSE_MAX_TABLELOG {
        return Err(ErrorCode::TableLogTooLarge); // Unsupported size
    }
    if table_log < fse_min_table_log(total, max_symbol_value) {
        return Err(ErrorCode::Generic); // Too small tableLog, compression potentially impossible
    }

    const RTB_TABLE: [u32; 8] = [0, 473195, 504333, 520860, 550000, 700000, 750000, 830000];
    let low_prob_count: i16 = if use_low_prob_count { -1 } else { 1 };
    let scale: u64 = 62 - u64::from(table_log);
    let step: u64 = (1u64 << 62) / (total as u32 as u64); // <== here, one division !
    let v_step: u64 = 1u64 << (scale - 20);
    let mut still_to_distribute: i32 = 1 << table_log;
    let mut largest = 0usize;
    let mut largest_p: i16 = 0;
    let low_threshold = (total >> table_log) as u32;

    for s in 0..=max_symbol_value as usize {
        if count[s] as usize == total {
            return Ok(0); // rle special case
        }
        if count[s] == 0 {
            normalized_counter[s] = 0;
            continue;
        }
        if count[s] <= low_threshold {
            normalized_counter[s] = low_prob_count;
            still_to_distribute -= 1;
        } else {
            let cs = u64::from(count[s]).wrapping_mul(step);
            let mut proba = (cs >> scale) as i16;
            if proba < 8 {
                let rest_to_beat = v_step * u64::from(RTB_TABLE[proba as usize]);
                proba += i16::from(cs - ((proba as u64) << scale) > rest_to_beat);
            }
            if proba > largest_p {
                largest_p = proba;
                largest = s;
            }
            normalized_counter[s] = proba;
            still_to_distribute -= i32::from(proba);
        }
    }
    if -still_to_distribute >= i32::from(normalized_counter[largest] >> 1) {
        // corner case, need another normalization method
        fse_normalize_m2(
            normalized_counter,
            table_log,
            count,
            total,
            max_symbol_value,
            low_prob_count,
        )?;
    } else {
        normalized_counter[largest] += still_to_distribute as i16;
    }
    Ok(table_log)
}

/// Port of `FSE_buildCTable_rle` (external/zstd/lib/compress/fse_compress.c:529-549
/// @ zstd v1.5.7): the table of a single symbol (the other cells are left as they were).
pub(crate) fn fse_build_ctable_rle(ct: &mut FseCTable, symbol_value: u8) {
    // header
    ct.set_u16(0, 0);
    ct.set_u16(1, u16::from(symbol_value));
    // Build table
    ct.set_u16(2, 0);
    ct.set_u16(3, 0); // just in case
    // Build Symbol Transformation Table (FSCTptr = ct + 2 words)
    let at = 2 + 2 * usize::from(symbol_value);
    ct.0[at + 1] = 0;
    ct.0[at] = 0;
}

/// Port of `FSE_CState_t` with `FSE_initCState`, `FSE_initCState2`, `FSE_encodeSymbol` and
/// `FSE_flushCState` (external/zstd/lib/common/fse.h:427-478 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy)]
pub(crate) struct FseCState {
    pub(crate) value: isize,
    pub(crate) state_log: u32,
    /// Where the table's `symbolTT` starts, in words: upstream's `symbolTT` pointer, set once
    /// by `FSE_initCState`.
    symbol_tt: usize,
}

impl FseCState {
    pub(crate) fn init(ct: &FseCTable) -> Self {
        let table_log = ct.table_log();
        FseCState {
            value: 1isize << table_log,
            state_log: table_log,
            symbol_tt: FseCTable::symbol_tt_base(table_log),
        }
    }

    /// `FSE_initCState2`: the first symbol (the last read) takes the smallest state.
    pub(crate) fn init2(ct: &FseCTable, symbol: u32) -> Self {
        let mut st = Self::init(ct);
        let (delta_find_state, delta_nb_bits) = ct.symbol_tt(symbol as usize);
        let nb_bits_out = delta_nb_bits.wrapping_add(1 << 15) >> 16;
        st.value = ((nb_bits_out << 16).wrapping_sub(delta_nb_bits)) as isize;
        let idx = (st.value >> nb_bits_out) + delta_find_state as isize;
        st.value = ct.state(idx as usize) as isize;
        st
    }

    #[inline(always)] // MEM_STATIC upstream, in fse.h
    pub(crate) fn encode_symbol(
        &mut self,
        bit_c: &mut BitCStream<'_>,
        ct: &FseCTable,
        symbol: u32,
    ) {
        // the symbol's entry and the next state from the pointers set at init
        let at = self.symbol_tt + 2 * symbol as usize;
        let (delta_find_state, delta_nb_bits) = (ct.0[at] as i32, ct.0[at + 1]);
        // (ptrdiff_t + U32: computed in 64 bits)
        let nb_bits_out = ((self.value as i64 + i64::from(delta_nb_bits)) >> 16) as u32;
        bit_c.add_bits(self.value as u64, nb_bits_out);
        let idx = (self.value >> nb_bits_out) + delta_find_state as isize;
        self.value = ct.state(idx as usize) as isize;
    }

    pub(crate) fn flush(&self, bit_c: &mut BitCStream<'_>) {
        bit_c.add_bits(self.value as u64, self.state_log);
        bit_c.flush_bits();
    }
}

/// Port of `FSE_compress_usingCTable_generic` (external/zstd/lib/compress/fse_compress.c:551-600
/// @ zstd v1.5.7): `src` encoded backwards with two interleaved states; 0 when it does not
/// fit.
fn fse_compress_using_ctable_generic(
    dst: &mut [u8],
    src: &[u8],
    ct: &FseCTable,
    fast: bool,
) -> usize {
    let mut src_size = src.len();
    let mut ip = src_size;

    // init
    if src_size <= 2 {
        return 0;
    }
    let Ok(mut bit_c) = BitCStream::init(dst) else {
        return 0; // not enough space available to write a bitstream
    };
    let flush = |b: &mut BitCStream<'_>| {
        if fast {
            b.flush_bits_fast();
        } else {
            b.flush_bits();
        }
    };

    let mut c_state1;
    let mut c_state2;
    if src_size & 1 != 0 {
        ip -= 1;
        c_state1 = FseCState::init2(ct, u32::from(src[ip]));
        ip -= 1;
        c_state2 = FseCState::init2(ct, u32::from(src[ip]));
        ip -= 1;
        c_state1.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        flush(&mut bit_c);
    } else {
        ip -= 1;
        c_state2 = FseCState::init2(ct, u32::from(src[ip]));
        ip -= 1;
        c_state1 = FseCState::init2(ct, u32::from(src[ip]));
    }

    // join to mod 4
    src_size -= 2;
    if src_size & 2 != 0 {
        // (64-bit register: room for 4 symbols)
        ip -= 1;
        c_state2.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        ip -= 1;
        c_state1.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        flush(&mut bit_c);
    }

    // 2 or 4 encoding per loop
    while ip > 0 {
        ip -= 1;
        c_state2.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        ip -= 1;
        c_state1.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        ip -= 1;
        c_state2.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        ip -= 1;
        c_state1.encode_symbol(&mut bit_c, ct, u32::from(src[ip]));
        flush(&mut bit_c);
    }

    c_state2.flush(&mut bit_c);
    c_state1.flush(&mut bit_c);
    bit_c.close()
}

/// Port of `FSE_compress_usingCTable` (external/zstd/lib/compress/fse_compress.c:602-611
/// @ zstd v1.5.7).
pub(crate) fn fse_compress_using_ctable(dst: &mut [u8], src: &[u8], ct: &FseCTable) -> usize {
    let fast = dst.len() >= fse_block_bound(src.len());
    fse_compress_using_ctable_generic(dst, src, ct, fast)
}
