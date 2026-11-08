// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `entropy_common.c` (external/zstd/lib/common/entropy_common.c:1-340 @ zstd v1.5.7):
//! the readers of FSE's normalized counts and of a Huffman tree's weights. The BMI2 builds of
//! these functions are the same code compiled for other instructions.

pub(crate) use crate::bmi2::{fse_read_ncount, huf_read_stats};
use crate::error_private::{ErrorCode, ZResult};
use crate::fse_decompress::{FSE_MAX_SYMBOL_VALUE, fse_decompress_wksp};
use crate::mem::{count_trailing_zeros32, highbit32, read_le32};

/// `FSE_MIN_TABLELOG` (external/zstd/lib/common/fse.h:616 @ zstd v1.5.7).
pub(crate) const FSE_MIN_TABLELOG: u32 = 5;
/// `FSE_TABLELOG_ABSOLUTE_MAX` (external/zstd/lib/common/fse.h:618 @ zstd v1.5.7).
pub(crate) const FSE_TABLELOG_ABSOLUTE_MAX: u32 = 15;
/// `HUF_TABLELOG_MAX` (external/zstd/lib/common/huf.h:37 @ zstd v1.5.7).
pub(crate) const HUF_TABLELOG_MAX: u32 = 12;

/// Port of `FSE_readNCount_body` and `FSE_readNCount` (external/zstd/lib/common/entropy_common.c:42-224
/// @ zstd v1.5.7): the normalized counts of an FSE table header; `max_sv` is the largest
/// symbol allowed on entry and the largest present on return. Returns the header's size.
#[inline(always)]
pub(crate) fn fse_read_ncount_body(
    normalized_counter: &mut [i16],
    max_sv: &mut u32,
    table_log: &mut u32,
    header: &[u8],
) -> ZResult<usize> {
    let hb_size = header.len();
    if hb_size < 8 {
        // This function only works when hbSize >= 8
        let mut buffer = [0u8; 8];
        buffer[..hb_size].copy_from_slice(header);
        let count_size = fse_read_ncount(normalized_counter, max_sv, table_log, &buffer)?;
        if count_size > hb_size {
            return Err(ErrorCode::CorruptionDetected);
        }
        return Ok(count_size);
    }

    let iend = hb_size as isize;
    let mut ip: isize = 0;
    let max_sv1 = *max_sv + 1;
    let mut previous0 = false;
    let mut charnum: u32 = 0;
    let rd = |ip: isize| read_le32(&header[ip as usize..]);

    // init
    // all symbols not present in NCount have a frequency of 0
    normalized_counter[..max_sv1 as usize].fill(0);
    let mut bit_stream = rd(ip);
    let mut nb_bits = (bit_stream & 0xF) as i32 + FSE_MIN_TABLELOG as i32; // extract tableLog
    if nb_bits > FSE_TABLELOG_ABSOLUTE_MAX as i32 {
        return Err(ErrorCode::TableLogTooLarge);
    }
    bit_stream >>= 4;
    let mut bit_count: i32 = 4;
    *table_log = nb_bits as u32;
    let mut remaining: i32 = (1 << nb_bits) + 1;
    let mut threshold: i32 = 1 << nb_bits;
    nb_bits += 1;

    loop {
        if previous0 {
            // Count the number of repeats. Each time the 2-bit repeat code is 0b11 there is
            // another repeat. Avoid UB by setting the high bit to 1.
            let mut repeats = (count_trailing_zeros32(!bit_stream | 0x8000_0000) >> 1) as i32;
            while repeats >= 12 {
                charnum += 3 * 12;
                if ip <= iend - 7 {
                    ip += 3;
                } else {
                    bit_count -= (8 * (iend - 7 - ip)) as i32;
                    bit_count &= 31;
                    ip = iend - 4;
                }
                bit_stream = rd(ip) >> bit_count;
                repeats = (count_trailing_zeros32(!bit_stream | 0x8000_0000) >> 1) as i32;
            }
            charnum += 3 * repeats as u32;
            bit_stream >>= 2 * repeats;
            bit_count += 2 * repeats;

            // Add the final repeat which isn't 0b11.
            charnum += bit_stream & 3;
            bit_count += 2;

            // This is an error, but break and return an error at the end, because returning
            // out of a loop makes it harder for the compiler to optimize.
            if charnum >= max_sv1 {
                break;
            }

            // We don't need to set the normalized count to 0 because we already memset the
            // whole buffer to 0.
            if ip <= iend - 7 || ip + (bit_count >> 3) as isize <= iend - 4 {
                ip += (bit_count >> 3) as isize;
                bit_count &= 7;
            } else {
                bit_count -= (8 * (iend - 4 - ip)) as i32;
                bit_count &= 31;
                ip = iend - 4;
            }
            bit_stream = rd(ip) >> bit_count;
        }
        {
            let max = (2 * threshold - 1) - remaining;
            let mut count: i32;

            if (bit_stream & (threshold - 1) as u32) < max as u32 {
                count = (bit_stream & (threshold - 1) as u32) as i32;
                bit_count += nb_bits - 1;
            } else {
                count = (bit_stream & (2 * threshold - 1) as u32) as i32;
                if count >= threshold {
                    count -= max;
                }
                bit_count += nb_bits;
            }

            count -= 1; // extra accuracy
            // When it matters (small blocks), this is a predictable branch, because we don't
            // use -1.
            if count >= 0 {
                remaining -= count;
            } else {
                remaining += count;
            }
            normalized_counter[charnum as usize] = count as i16;
            charnum += 1;
            previous0 = count == 0;

            if remaining < threshold {
                // This branch can be folded into the threshold update condition because we
                // know that threshold > 1.
                if remaining <= 1 {
                    break;
                }
                nb_bits = highbit32(remaining as u32) as i32 + 1;
                threshold = 1 << (nb_bits - 1);
            }
            if charnum >= max_sv1 {
                break;
            }

            if ip <= iend - 7 || ip + (bit_count >> 3) as isize <= iend - 4 {
                ip += (bit_count >> 3) as isize;
                bit_count &= 7;
            } else {
                bit_count -= (8 * (iend - 4 - ip)) as i32;
                bit_count &= 31;
                ip = iend - 4;
            }
            bit_stream = rd(ip) >> bit_count;
        }
    }
    if remaining != 1 {
        return Err(ErrorCode::CorruptionDetected);
    }
    // Only possible when there are too many zeros.
    if charnum > max_sv1 {
        return Err(ErrorCode::MaxSymbolValueTooSmall);
    }
    if bit_count > 32 {
        return Err(ErrorCode::CorruptionDetected);
    }
    *max_sv = charnum - 1;

    ip += ((bit_count + 7) >> 3) as isize;
    Ok(ip as usize)
}

/// `HUF_READ_STATS_WORKSPACE_SIZE` (external/zstd/lib/common/huf.h:181-182 @ zstd v1.5.7), in
/// bytes: `FSE_DECOMPRESS_WKSP_SIZE_U32 (6, HUF_TABLELOG_MAX - 1)` words.
pub(crate) const HUF_READ_STATS_WORKSPACE_SIZE: usize =
    crate::fse_decompress::fse_decompress_wksp_size(6, HUF_TABLELOG_MAX - 1);

/// Port of `HUF_readStats_body` and `HUF_readStats_wksp`
/// (external/zstd/lib/common/entropy_common.c:242-340 @ zstd v1.5.7): a Huffman tree's
/// weights, stored directly (4 bits each) or FSE compressed, with the last weight implied;
/// `rank_stats` counts each weight. `wksp_size` is the size of upstream's workspace, which
/// decides what FSE header fits. Returns the size read.
#[inline(always)]
pub(crate) fn huf_read_stats_body(
    huff_weight: &mut [u8],
    rank_stats: &mut [u32],
    nb_symbols: &mut u32,
    table_log_out: &mut u32,
    src: &[u8],
    wksp_size: usize,
) -> ZResult<usize> {
    let hw_size = huff_weight.len();
    let src_size = src.len();
    if src_size == 0 {
        return Err(ErrorCode::SrcSizeWrong);
    }
    let mut i_size = usize::from(src[0]);
    let o_size: usize;

    if i_size >= 128 {
        // special header
        o_size = i_size - 127;
        i_size = o_size.div_ceil(2);
        if i_size + 1 > src_size {
            return Err(ErrorCode::SrcSizeWrong);
        }
        if o_size >= hw_size {
            return Err(ErrorCode::CorruptionDetected);
        }
        let ip = &src[1..];
        let mut n = 0;
        while n < o_size {
            huff_weight[n] = ip[n / 2] >> 4;
            huff_weight[n + 1] = ip[n / 2] & 15;
            n += 2;
        }
    } else {
        // header compressed with FSE (normal case)
        if i_size + 1 > src_size {
            return Err(ErrorCode::SrcSizeWrong);
        }
        // max (hwSize-1) values decoded, as last one is implied
        o_size = fse_decompress_wksp(
            &mut huff_weight[..hw_size - 1],
            &src[1..1 + i_size],
            6,
            wksp_size,
        )?;
    }

    // collect weight stats
    rank_stats[..(HUF_TABLELOG_MAX + 1) as usize].fill(0);
    let mut weight_total: u32 = 0;
    for &w in &huff_weight[..o_size] {
        if u32::from(w) > HUF_TABLELOG_MAX {
            return Err(ErrorCode::CorruptionDetected);
        }
        rank_stats[usize::from(w)] += 1;
        weight_total += (1 << w) >> 1;
    }
    if weight_total == 0 {
        return Err(ErrorCode::CorruptionDetected);
    }

    // get last non-null symbol weight (implied, total must be 2^n)
    let table_log = highbit32(weight_total) + 1;
    if table_log > HUF_TABLELOG_MAX {
        return Err(ErrorCode::CorruptionDetected);
    }
    *table_log_out = table_log;
    // determine last weight
    let total = 1u32 << table_log;
    let rest = total - weight_total;
    let verif = 1u32 << highbit32(rest);
    let last_weight = highbit32(rest) + 1;
    if verif != rest {
        // last value must be a clean power of 2
        return Err(ErrorCode::CorruptionDetected);
    }
    huff_weight[o_size] = last_weight as u8;
    rank_stats[last_weight as usize] += 1;

    // check tree construction validity
    if rank_stats[1] < 2 || (rank_stats[1] & 1) != 0 {
        // by construction : at least 2 elts of rank 1, must be even
        return Err(ErrorCode::CorruptionDetected);
    }

    // results
    *nb_symbols = (o_size + 1) as u32;
    Ok(i_size + 1)
}

const _: () = assert!(FSE_MAX_SYMBOL_VALUE == 255);
