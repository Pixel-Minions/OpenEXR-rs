// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `huf_compress.c` (external/zstd/lib/compress/huf_compress.c:1-1464 @ zstd v1.5.7):
//! Huffman tables and the one- and four-stream literal encoders.
//!
//! A `HUF_CElt` is a `size_t`: the code length in the low 8 bits, the code in the top bits.
//! A CTable keeps upstream's layout, its header in entry 0, because a CTable outlives its
//! block (the next block may reuse it); entries above its largest symbol are never read
//! without a dictionary, which Core doesn't use. Upstream's shared workspaces are typed
//! here: each scratch area upstream reads is written first in the same call, so fresh
//! scratch reads the same values. `HUF_readCTable` and `HUF_getNbBitsFromCTable`, which the
//! optimal parser calls only for a dictionary's Huffman table, are n/a (no dictionaries).

pub(crate) use crate::bmi2::huf_compress1x_using_ctable;
use crate::error_private::{ErrorCode, ZResult};
use crate::fse_compress::{
    FseCTable, fse_build_ctable, fse_build_ctable_workspace_size, fse_compress_using_ctable,
    fse_normalize_count, fse_optimal_table_log, fse_optimal_table_log_internal, fse_write_ncount,
};
use crate::hist::{hist_count, hist_count_simple};
use crate::mem::highbit32;

/// `HUF_BLOCKSIZE_MAX` (external/zstd/lib/common/huf.h:25 @ zstd v1.5.7).
pub(crate) const HUF_BLOCKSIZE_MAX: usize = 128 * 1024;
/// `HUF_TABLELOG_MAX` (external/zstd/lib/common/huf.h:37 @ zstd v1.5.7).
pub(crate) const HUF_TABLELOG_MAX: u32 = 12;
/// `HUF_TABLELOG_DEFAULT` (external/zstd/lib/common/huf.h:38 @ zstd v1.5.7).
pub(crate) const HUF_TABLELOG_DEFAULT: u32 = 11;
/// `HUF_SYMBOLVALUE_MAX` (external/zstd/lib/common/huf.h:39 @ zstd v1.5.7).
pub(crate) const HUF_SYMBOLVALUE_MAX: u32 = 255;
/// `HUF_CTABLE_SIZE_ST (HUF_SYMBOLVALUE_MAX)` (external/zstd/lib/common/huf.h:58
/// @ zstd v1.5.7): the entries of a CTable, the header and one per symbol.
pub(crate) const HUF_CTABLE_SIZE_ST: usize = HUF_SYMBOLVALUE_MAX as usize + 2;

/// `HUF_flags_bmi2` (external/zstd/lib/common/huf.h:85 @ zstd v1.5.7).
pub(crate) const HUF_FLAGS_BMI2: i32 = 1 << 0;
/// `HUF_flags_optimalDepth` (external/zstd/lib/common/huf.h:90 @ zstd v1.5.7).
pub(crate) const HUF_FLAGS_OPTIMAL_DEPTH: i32 = 1 << 1;
/// `HUF_flags_preferRepeat` (external/zstd/lib/common/huf.h:95 @ zstd v1.5.7).
pub(crate) const HUF_FLAGS_PREFER_REPEAT: i32 = 1 << 2;
/// `HUF_flags_suspectUncompressible` (external/zstd/lib/common/huf.h:100 @ zstd v1.5.7).
pub(crate) const HUF_FLAGS_SUSPECT_UNCOMPRESSIBLE: i32 = 1 << 3;

/// `HUF_compressBound` (external/zstd/lib/compress/huf_compress.c:821 @ zstd v1.5.7):
/// `HUF_COMPRESSBOUND`, `HUF_CTABLEBOUND + HUF_BLOCKBOUND (size)`.
pub(crate) const fn huf_compress_bound(size: usize) -> usize {
    129 + (size + (size >> 8) + 8)
}

/// A Huffman CTable (`HUF_CElt[HUF_CTABLE_SIZE_ST (HUF_SYMBOLVALUE_MAX)]`).
pub(crate) type HufCTable = [u64; HUF_CTABLE_SIZE_ST];

/// Port of `HUF_repeat` (external/zstd/lib/common/huf.h:139-143 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HufRepeat {
    /// Cannot use the previous table.
    None,
    /// Can use the previous table, but it must be checked.
    Check,
    /// Can use the previous table, which is assumed valid.
    Valid,
}

/// Port of `HUF_nbStreams_e` (external/zstd/lib/compress/huf_compress.c:1222 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NbStreams {
    Single,
    Four,
}

/// Port of `nodeElt` (external/zstd/lib/compress/huf_compress.c:47-52 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
struct NodeElt {
    count: u32,
    parent: u16,
    byte: u8,
    nb_bits: u8,
}

/// `huffNodeTable` (external/zstd/lib/compress/huf_compress.c:505 @ zstd v1.5.7).
const HUFF_NODE_TABLE_SIZE: usize = 2 * (HUF_SYMBOLVALUE_MAX as usize + 1);

/// `MAX_FSE_TABLELOG_FOR_HUFF_HEADER` (external/zstd/lib/compress/huf_compress.c:137
/// @ zstd v1.5.7).
const MAX_FSE_TABLELOG_FOR_HUFF_HEADER: u32 = 6;

/// Port of `HUF_compressWeights` (external/zstd/lib/compress/huf_compress.c:146-186
/// @ zstd v1.5.7): the weights FSE-compressed into `dst`; 0 when not compressible, 1 when
/// they are all one value.
fn huf_compress_weights(dst: &mut [u8], weight_table: &[u8]) -> ZResult<usize> {
    let wt_size = weight_table.len();
    let mut max_symbol_value = HUF_TABLELOG_MAX;
    let mut count = [0u32; HUF_TABLELOG_MAX as usize + 1];
    let mut norm = [0i16; HUF_TABLELOG_MAX as usize + 1];
    let mut ctable = FseCTable::new(MAX_FSE_TABLELOG_FOR_HUFF_HEADER, HUF_TABLELOG_MAX);

    // init conditions
    if wt_size <= 1 {
        return Ok(0); // Not compressible
    }

    // Scan input and build symbol stats
    {
        let max_count = hist_count_simple(&mut count, &mut max_symbol_value, weight_table); // never fails
        if max_count as usize == wt_size {
            return Ok(1); // only a single symbol in src : rle
        }
        if max_count == 1 {
            return Ok(0); // each symbol present maximum once => not compressible
        }
    }

    let table_log =
        fse_optimal_table_log(MAX_FSE_TABLELOG_FOR_HUFF_HEADER, wt_size, max_symbol_value);
    fse_normalize_count(
        &mut norm,
        table_log,
        &count,
        wt_size,
        max_symbol_value,
        false,
    )?;

    // Write table description header
    let h_size = fse_write_ncount(dst, &norm, max_symbol_value, table_log)?;
    let mut op = h_size;

    // Compress
    fse_build_ctable(
        &mut ctable,
        &norm,
        max_symbol_value,
        table_log,
        fse_build_ctable_workspace_size(HUF_TABLELOG_MAX, MAX_FSE_TABLELOG_FOR_HUFF_HEADER),
    )?;
    let c_size = fse_compress_using_ctable(&mut dst[op..], weight_table, &ctable);
    if c_size == 0 {
        return Ok(0); // not enough space for compressed data
    }
    op += c_size;
    Ok(op)
}

/// Port of `HUF_getNbBits` (external/zstd/lib/compress/huf_compress.c:188-191 @ zstd v1.5.7).
#[inline]
fn get_nb_bits(elt: u64) -> u64 {
    elt & 0xFF
}

/// Port of `HUF_getValue` (external/zstd/lib/compress/huf_compress.c:198-201 @ zstd v1.5.7).
#[inline]
fn get_value(elt: u64) -> u64 {
    elt & !0xFFu64
}

/// Port of `HUF_setNbBits` (external/zstd/lib/compress/huf_compress.c:208-212 @ zstd v1.5.7).
#[inline]
fn set_nb_bits(elt: &mut u64, nb_bits: u64) {
    *elt = nb_bits;
}

/// Port of `HUF_setValue` (external/zstd/lib/compress/huf_compress.c:214-221 @ zstd v1.5.7).
#[inline]
fn set_value(elt: &mut u64, value: u64) {
    let nb_bits = get_nb_bits(*elt);
    if nb_bits > 0 {
        *elt |= value << (64 - nb_bits);
    }
}

/// Port of `HUF_readCTableHeader` (external/zstd/lib/compress/huf_compress.c:223-228
/// @ zstd v1.5.7): the table log and the largest symbol.
#[inline]
pub(crate) fn read_ctable_header(ctable: &HufCTable) -> (u32, u32) {
    ((ctable[0] & 0xFF) as u32, ((ctable[0] >> 8) & 0xFF) as u32)
}

/// Port of `HUF_writeCTableHeader` (external/zstd/lib/compress/huf_compress.c:230-240
/// @ zstd v1.5.7).
#[inline]
fn write_ctable_header(ctable: &mut HufCTable, table_log: u32, max_symbol_value: u32) {
    ctable[0] = u64::from(table_log as u8) | (u64::from(max_symbol_value as u8) << 8);
}

/// Port of `HUF_writeCTable_wksp` (external/zstd/lib/compress/huf_compress.c:248-289
/// @ zstd v1.5.7): the table's description, its weights FSE-compressed or as 4-bit values.
pub(crate) fn huf_write_ctable(
    dst: &mut [u8],
    ctable: &HufCTable,
    max_symbol_value: u32,
    huff_log: u32,
) -> ZResult<usize> {
    let ct = &ctable[1..];
    let max_dst_size = dst.len();
    let mut bits_to_weight = [0u8; HUF_TABLELOG_MAX as usize + 1];
    let mut huff_weight = [0u8; HUF_SYMBOLVALUE_MAX as usize];

    // check conditions
    if max_symbol_value > HUF_SYMBOLVALUE_MAX {
        return Err(ErrorCode::MaxSymbolValueTooLarge);
    }

    // convert to weight
    bits_to_weight[0] = 0;
    for n in 1..huff_log + 1 {
        bits_to_weight[n as usize] = (huff_log + 1 - n) as u8;
    }
    for n in 0..max_symbol_value as usize {
        huff_weight[n] = bits_to_weight[get_nb_bits(ct[n]) as usize];
    }

    // attempt weights compression by FSE
    if max_dst_size < 1 {
        return Err(ErrorCode::DstSizeTooSmall);
    }
    {
        let h_size =
            huf_compress_weights(&mut dst[1..], &huff_weight[..max_symbol_value as usize])?;
        if (h_size > 1) & (h_size < (max_symbol_value / 2) as usize) {
            // FSE compressed
            dst[0] = h_size as u8;
            return Ok(h_size + 1);
        }
    }

    // write raw values as 4-bits (max : 15)
    if max_symbol_value > (256 - 128) {
        return Err(ErrorCode::Generic); // should not happen : likely means source cannot be compressed
    }
    if ((max_symbol_value + 1) / 2) as usize + 1 > max_dst_size {
        return Err(ErrorCode::DstSizeTooSmall); // not enough space within dst buffer
    }
    dst[0] = (128 /*special case*/ + (max_symbol_value - 1)) as u8;
    huff_weight[max_symbol_value as usize] = 0; // to be sure it doesn't cause msan issue in final combination
    for n in (0..max_symbol_value as usize).step_by(2) {
        dst[(n / 2) + 1] = (huff_weight[n] << 4).wrapping_add(huff_weight[n + 1]);
    }
    Ok(((max_symbol_value + 1) / 2) as usize + 1)
}

/// Port of `HUF_setMaxHeight` (external/zstd/lib/compress/huf_compress.c:376-498
/// @ zstd v1.5.7): the tree limited to `target_nb_bits`, still canonical. `nodes` is
/// upstream's `huffNode0`, `huffNode[i]` is `nodes[i + 1]`.
fn huf_set_max_height(nodes: &mut [NodeElt], last_non_null: u32, target_nb_bits: u32) -> u32 {
    let h = |i: i32| (i + 1) as usize;
    let largest_bits = u32::from(nodes[h(last_non_null as i32)].nb_bits);
    // early exit : no elt > targetNbBits, so the tree is already valid.
    if largest_bits <= target_nb_bits {
        return largest_bits;
    }

    // there are several too large elements (at least >= 2)
    let mut total_cost: i32 = 0;
    let base_cost: u32 = 1 << (largest_bits - target_nb_bits);
    let mut n = last_non_null as i32;

    // Adjust any ranks > targetNbBits to targetNbBits.
    while u32::from(nodes[h(n)].nb_bits) > target_nb_bits {
        total_cost = (total_cost as u32).wrapping_add(
            base_cost.wrapping_sub(1 << (largest_bits - u32::from(nodes[h(n)].nb_bits))),
        ) as i32;
        nodes[h(n)].nb_bits = target_nb_bits as u8;
        n -= 1;
    }
    // n stops at huffNode[n].nbBits <= targetNbBits
    // n end at index of smallest symbol using < targetNbBits
    while u32::from(nodes[h(n)].nb_bits) == target_nb_bits {
        n -= 1;
    }

    // renorm totalCost from 2^largestBits to 2^targetNbBits
    total_cost >>= largest_bits - target_nb_bits;

    // repay normalized cost
    {
        const NO_SYMBOL: u32 = 0xF0F0F0F0;
        let mut rank_last = [NO_SYMBOL; HUF_TABLELOG_MAX as usize + 2];

        // Get pos of last (smallest = lowest cum. count) symbol per rank
        {
            let mut current_nb_bits = target_nb_bits;
            let mut pos = n;
            while pos >= 0 {
                if u32::from(nodes[h(pos)].nb_bits) >= current_nb_bits {
                    pos -= 1;
                    continue;
                }
                current_nb_bits = u32::from(nodes[h(pos)].nb_bits); // < targetNbBits
                rank_last[(target_nb_bits - current_nb_bits) as usize] = pos as u32;
                pos -= 1;
            }
        }

        while total_cost > 0 {
            // Try to reduce the next power of 2 above totalCost because we gain back half the rank.
            let mut n_bits_to_decrease = highbit32(total_cost as u32) + 1;
            while n_bits_to_decrease > 1 {
                let high_pos = rank_last[n_bits_to_decrease as usize];
                let low_pos = rank_last[n_bits_to_decrease as usize - 1];
                if high_pos == NO_SYMBOL {
                    n_bits_to_decrease -= 1;
                    continue;
                }
                // Decrease highPos if no symbols of lowPos or if it is not cheaper to remove
                // 2 lowPos than highPos.
                if low_pos == NO_SYMBOL {
                    break;
                }
                {
                    let high_total = nodes[h(high_pos as i32)].count;
                    let low_total = 2u32.wrapping_mul(nodes[h(low_pos as i32)].count);
                    if high_total <= low_total {
                        break;
                    }
                }
                n_bits_to_decrease -= 1;
            }
            // only triggered when no more rank 1 symbol left => find closest one
            // HUF_MAX_TABLELOG test just to please gcc 5+; but it should not be necessary
            while (n_bits_to_decrease <= HUF_TABLELOG_MAX)
                && (rank_last[n_bits_to_decrease as usize] == NO_SYMBOL)
            {
                n_bits_to_decrease += 1;
            }
            // Increase the number of bits to gain back half the rank cost.
            total_cost -= 1 << (n_bits_to_decrease - 1);
            let rl = rank_last[n_bits_to_decrease as usize] as i32;
            nodes[h(rl)].nb_bits = nodes[h(rl)].nb_bits.wrapping_add(1);

            // Fix up the new rank.
            if rank_last[n_bits_to_decrease as usize - 1] == NO_SYMBOL {
                rank_last[n_bits_to_decrease as usize - 1] = rank_last[n_bits_to_decrease as usize];
            }

            // Fix up the old rank.
            if rank_last[n_bits_to_decrease as usize] == 0 {
                // special case, reached largest symbol
                rank_last[n_bits_to_decrease as usize] = NO_SYMBOL;
            } else {
                rank_last[n_bits_to_decrease as usize] -= 1;
                if u32::from(nodes[h(rank_last[n_bits_to_decrease as usize] as i32)].nb_bits)
                    != target_nb_bits.wrapping_sub(n_bits_to_decrease)
                {
                    rank_last[n_bits_to_decrease as usize] = NO_SYMBOL; // this rank is now empty
                }
            }
        } // while (totalCost > 0)

        // If we've removed too much weight, then we have to add it back.
        while total_cost < 0 {
            // Sometimes, cost correction overshoot
            // special case : no rank 1 symbol (using targetNbBits-1);
            // let's create one from largest rank 0 (using targetNbBits).
            if rank_last[1] == NO_SYMBOL {
                while u32::from(nodes[h(n)].nb_bits) == target_nb_bits {
                    n -= 1;
                }
                nodes[h(n + 1)].nb_bits = nodes[h(n + 1)].nb_bits.wrapping_sub(1);
                rank_last[1] = (n + 1) as u32;
                total_cost += 1;
                continue;
            }
            let p = (rank_last[1] + 1) as i32;
            nodes[h(p)].nb_bits = nodes[h(p)].nb_bits.wrapping_sub(1);
            rank_last[1] += 1;
            total_cost += 1;
        }
    } // repay normalized cost

    target_nb_bits
}

/// Port of `rankPos` (external/zstd/lib/compress/huf_compress.c:500-503 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
struct RankPos {
    base: u16,
    curr: u16,
}

/// `RANK_POSITION_TABLE_SIZE` (external/zstd/lib/compress/huf_compress.c:508 @ zstd v1.5.7).
const RANK_POSITION_TABLE_SIZE: usize = 192;
/// `RANK_POSITION_MAX_COUNT_LOG` (external/zstd/lib/compress/huf_compress.c:523 @ zstd v1.5.7).
const RANK_POSITION_MAX_COUNT_LOG: u32 = 32;
/// `RANK_POSITION_LOG_BUCKETS_BEGIN` (external/zstd/lib/compress/huf_compress.c:524
/// @ zstd v1.5.7): 158.
const RANK_POSITION_LOG_BUCKETS_BEGIN: u32 =
    (RANK_POSITION_TABLE_SIZE as u32 - 1) - RANK_POSITION_MAX_COUNT_LOG - 1;
/// `RANK_POSITION_DISTINCT_COUNT_CUTOFF` (external/zstd/lib/compress/huf_compress.c:525
/// @ zstd v1.5.7): 166.
const RANK_POSITION_DISTINCT_COUNT_CUTOFF: u32 =
    RANK_POSITION_LOG_BUCKETS_BEGIN + (31 - RANK_POSITION_LOG_BUCKETS_BEGIN.leading_zeros());

/// Port of `HUF_getIndex` (external/zstd/lib/compress/huf_compress.c:530-534 @ zstd v1.5.7):
/// the bucket of a count.
#[inline]
fn huf_get_index(count: u32) -> u32 {
    if count < RANK_POSITION_DISTINCT_COUNT_CUTOFF {
        count
    } else {
        highbit32(count) + RANK_POSITION_LOG_BUCKETS_BEGIN
    }
}

/// Port of `HUF_insertionSort` (external/zstd/lib/compress/huf_compress.c:555-568
/// @ zstd v1.5.7): by descending count.
fn huf_insertion_sort(huff_node: &mut [NodeElt], low: i32, high: i32) {
    let size = high - low + 1;
    let huff_node = &mut huff_node[low as usize..];
    for i in 1..size {
        let key = huff_node[i as usize];
        let mut j = i - 1;
        while j >= 0 && huff_node[j as usize].count < key.count {
            huff_node[(j + 1) as usize] = huff_node[j as usize];
            j -= 1;
        }
        huff_node[(j + 1) as usize] = key;
    }
}

/// Port of `HUF_quickSortPartition` (external/zstd/lib/compress/huf_compress.c:571-586
/// @ zstd v1.5.7).
fn huf_quick_sort_partition(arr: &mut [NodeElt], low: i32, high: i32) -> i32 {
    // Simply select rightmost element as pivot.
    let pivot = arr[high as usize].count;
    let mut i = low - 1;
    let mut j = low;
    while j < high {
        if arr[j as usize].count > pivot {
            i += 1;
            arr.swap(i as usize, j as usize);
        }
        j += 1;
    }
    arr.swap((i + 1) as usize, high as usize);
    i + 1
}

/// Port of `HUF_simpleQuickSort` (external/zstd/lib/compress/huf_compress.c:591-607
/// @ zstd v1.5.7): by descending count, recursing on the smaller side.
fn huf_simple_quick_sort(arr: &mut [NodeElt], mut low: i32, mut high: i32) {
    const K_INSERTION_SORT_THRESHOLD: i32 = 8;
    if high - low < K_INSERTION_SORT_THRESHOLD {
        huf_insertion_sort(arr, low, high);
        return;
    }
    while low < high {
        let idx = huf_quick_sort_partition(arr, low, high);
        if idx - low < high - idx {
            huf_simple_quick_sort(arr, low, idx - 1);
            low = idx + 1;
        } else {
            huf_simple_quick_sort(arr, idx + 1, high);
            high = idx - 1;
        }
    }
}

/// Port of `HUF_sort` (external/zstd/lib/compress/huf_compress.c:620-665 @ zstd v1.5.7): the
/// symbols `[0, max_symbol_value]` by decreasing count, bucketed, each bucket of large counts
/// quicksorted; fills `count` and `byte` only.
fn huf_sort(huff_node: &mut [NodeElt], count: &[u32], max_symbol_value: u32) {
    let max_symbol_value1 = max_symbol_value + 1;
    let mut rank_position = [RankPos::default(); RANK_POSITION_TABLE_SIZE];

    // Compute base and set curr to base.
    for n in 0..max_symbol_value1 {
        let lower_rank = huf_get_index(count[n as usize]);
        rank_position[lower_rank as usize].base += 1;
    }
    // Set up the rankPosition table
    for n in (1..RANK_POSITION_TABLE_SIZE).rev() {
        rank_position[n - 1].base = rank_position[n - 1]
            .base
            .wrapping_add(rank_position[n].base);
        rank_position[n - 1].curr = rank_position[n - 1].base;
    }
    // Insert each symbol into their appropriate bucket, setting up rankPosition table.
    for n in 0..max_symbol_value1 {
        let c = count[n as usize];
        let r = huf_get_index(c) + 1;
        let pos = rank_position[r as usize].curr;
        rank_position[r as usize].curr += 1;
        huff_node[pos as usize].count = c;
        huff_node[pos as usize].byte = n as u8;
    }
    // Sort each bucket.
    for n in RANK_POSITION_DISTINCT_COUNT_CUTOFF as usize..RANK_POSITION_TABLE_SIZE - 1 {
        let bucket_size = i32::from(rank_position[n].curr) - i32::from(rank_position[n].base);
        let bucket_start_idx = usize::from(rank_position[n].base);
        if bucket_size > 1 {
            huf_simple_quick_sort(&mut huff_node[bucket_start_idx..], 0, bucket_size - 1);
        }
    }
}

/// `STARTNODE` (external/zstd/lib/compress/huf_compress.c:672 @ zstd v1.5.7).
const STARTNODE: i32 = HUF_SYMBOLVALUE_MAX as i32 + 1;

/// Port of `HUF_buildTree` (external/zstd/lib/compress/huf_compress.c:681-718 @ zstd v1.5.7):
/// an unlimited-depth Huffman tree over the sorted nodes; returns the smallest node. `nodes`
/// is upstream's `huffNode0`, `huffNode[i]` is `nodes[i + 1]`.
fn huf_build_tree(nodes: &mut [NodeElt], max_symbol_value: u32) -> i32 {
    let h = |i: i32| (i + 1) as usize;
    let mut node_nb = STARTNODE;

    // init for parents
    let mut non_null_rank = max_symbol_value as i32;
    while nodes[h(non_null_rank)].count == 0 {
        non_null_rank -= 1;
    }
    let mut low_s = non_null_rank;
    let node_root = node_nb + low_s - 1;
    let mut low_n = node_nb;
    nodes[h(node_nb)].count = nodes[h(low_s)]
        .count
        .wrapping_add(nodes[h(low_s - 1)].count);
    nodes[h(low_s)].parent = node_nb as u16;
    nodes[h(low_s - 1)].parent = node_nb as u16;
    node_nb += 1;
    low_s -= 2;
    for n in node_nb..=node_root {
        nodes[h(n)].count = 1u32 << 30;
    }
    nodes[0].count = 1u32 << 31; // fake entry, strong barrier

    // create parents
    while node_nb <= node_root {
        let n1 = if nodes[h(low_s)].count < nodes[h(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        let n2 = if nodes[h(low_s)].count < nodes[h(low_n)].count {
            low_s -= 1;
            low_s + 1
        } else {
            low_n += 1;
            low_n - 1
        };
        nodes[h(node_nb)].count = nodes[h(n1)].count.wrapping_add(nodes[h(n2)].count);
        nodes[h(n1)].parent = node_nb as u16;
        nodes[h(n2)].parent = node_nb as u16;
        node_nb += 1;
    }

    // distribute weights (unlimited tree height)
    nodes[h(node_root)].nb_bits = 0;
    let mut n = node_root - 1;
    while n >= STARTNODE {
        let parent = i32::from(nodes[h(n)].parent);
        nodes[h(n)].nb_bits = nodes[h(parent)].nb_bits.wrapping_add(1);
        n -= 1;
    }
    for n in 0..=non_null_rank {
        let parent = i32::from(nodes[h(n)].parent);
        nodes[h(n)].nb_bits = nodes[h(parent)].nb_bits.wrapping_add(1);
    }
    non_null_rank
}

/// Port of `HUF_buildCTableFromTree` (external/zstd/lib/compress/huf_compress.c:730-753
/// @ zstd v1.5.7): canonical codes, in symbol order within each length.
fn huf_build_ctable_from_tree(
    ctable: &mut HufCTable,
    huff_node: &[NodeElt],
    non_null_rank: i32,
    max_symbol_value: u32,
    max_nb_bits: u32,
) {
    let mut nb_per_rank = [0u16; HUF_TABLELOG_MAX as usize + 1];
    let mut val_per_rank = [0u16; HUF_TABLELOG_MAX as usize + 1];
    let alphabet_size = (max_symbol_value + 1) as usize;
    for node in &huff_node[..=non_null_rank as usize] {
        nb_per_rank[usize::from(node.nb_bits)] += 1;
    }
    // determine starting value per rank
    {
        let mut min: u16 = 0;
        for n in (1..=max_nb_bits as usize).rev() {
            val_per_rank[n] = min; // get starting value within each rank
            min = min.wrapping_add(nb_per_rank[n]);
            min >>= 1;
        }
    }
    {
        let ct = &mut ctable[1..];
        for node in &huff_node[..alphabet_size] {
            // push nbBits per symbol, symbol order
            set_nb_bits(&mut ct[usize::from(node.byte)], u64::from(node.nb_bits));
        }
        for elt in &mut ct[..alphabet_size] {
            // assign value within rank, symbol order
            let r = get_nb_bits(*elt) as usize;
            set_value(elt, u64::from(val_per_rank[r]));
            val_per_rank[r] = val_per_rank[r].wrapping_add(1);
        }
    }
    write_ctable_header(ctable, max_nb_bits, max_symbol_value);
}

/// Port of `HUF_buildCTable_wksp` (external/zstd/lib/compress/huf_compress.c:755-791
/// @ zstd v1.5.7): the table of `count`, at most `max_nb_bits` long; returns the longest code.
pub(crate) fn huf_build_ctable(
    ctable: &mut HufCTable,
    count: &[u32],
    max_symbol_value: u32,
    max_nb_bits: u32,
) -> ZResult<u32> {
    let mut max_nb_bits = max_nb_bits;
    // safety checks
    if max_nb_bits == 0 {
        max_nb_bits = HUF_TABLELOG_DEFAULT;
    }
    if max_symbol_value > HUF_SYMBOLVALUE_MAX {
        return Err(ErrorCode::MaxSymbolValueTooLarge);
    }
    let mut nodes = [NodeElt::default(); HUFF_NODE_TABLE_SIZE];

    // sort, decreasing order
    huf_sort(&mut nodes[1..], count, max_symbol_value);

    // build tree
    let non_null_rank = huf_build_tree(&mut nodes, max_symbol_value);

    // determine and enforce maxTableLog
    max_nb_bits = huf_set_max_height(&mut nodes, non_null_rank as u32, max_nb_bits);
    if max_nb_bits > HUF_TABLELOG_MAX {
        return Err(ErrorCode::Generic); // check fit into table
    }

    huf_build_ctable_from_tree(
        ctable,
        &nodes[1..],
        non_null_rank,
        max_symbol_value,
        max_nb_bits,
    );
    Ok(max_nb_bits)
}

/// Port of `HUF_estimateCompressedSize` (external/zstd/lib/compress/huf_compress.c:793-802
/// @ zstd v1.5.7): the bytes `count` takes with the table.
pub(crate) fn huf_estimate_compressed_size(
    ctable: &HufCTable,
    count: &[u32],
    max_symbol_value: u32,
) -> usize {
    let ct = &ctable[1..];
    let mut nb_bits: usize = 0;
    for s in 0..=max_symbol_value as usize {
        nb_bits =
            nb_bits.wrapping_add((get_nb_bits(ct[s]) as usize).wrapping_mul(count[s] as usize));
    }
    nb_bits >> 3
}

/// Port of `HUF_validateCTable` (external/zstd/lib/compress/huf_compress.c:804-819
/// @ zstd v1.5.7): whether the table codes every symbol `count` has.
pub(crate) fn huf_validate_ctable(
    ctable: &HufCTable,
    count: &[u32],
    max_symbol_value: u32,
) -> bool {
    let (_, header_max_symbol_value) = read_ctable_header(ctable);
    let ct = &ctable[1..];
    let mut bad = false;
    if header_max_symbol_value < max_symbol_value {
        return false;
    }
    for s in 0..=max_symbol_value as usize {
        bad |= (count[s] != 0) & (get_nb_bits(ct[s]) == 0);
    }
    !bad
}

/// Port of `HUF_CStream_t` (external/zstd/lib/compress/huf_compress.c:843-850 @ zstd v1.5.7):
/// Huffman's bit stream, built from the top bits and right-shifted, with a second container
/// that merges into the first.
struct HufCStream<'a> {
    bit_container: [u64; 2],
    bit_pos: [u64; 2],
    dst: &'a mut [u8],
    ptr: usize,
    end_ptr: usize,
}

impl<'a> HufCStream<'a> {
    /// Port of `HUF_initCStream` (external/zstd/lib/compress/huf_compress.c:856-865
    /// @ zstd v1.5.7).
    fn init(dst: &'a mut [u8]) -> ZResult<Self> {
        let dst_capacity = dst.len();
        if dst_capacity <= 8 {
            return Err(ErrorCode::DstSizeTooSmall);
        }
        Ok(HufCStream {
            bit_container: [0; 2],
            bit_pos: [0; 2],
            dst,
            ptr: 0,
            end_ptr: dst_capacity - 8,
        })
    }

    /// Port of `HUF_addBits` (external/zstd/lib/compress/huf_compress.c:877-909 @ zstd v1.5.7):
    /// `K_FAST` ors in the whole element, its low bits dirty.
    #[inline(always)]
    fn add_bits<const K_FAST: bool>(&mut self, elt: u64, idx: usize) {
        self.bit_container[idx] >>= get_nb_bits(elt);
        self.bit_container[idx] |= if K_FAST { elt } else { get_value(elt) };
        // We only read the low 8 bits of bitC->bitPos[idx] so it doesn't matter that the
        // high bits have noise from the value.
        self.bit_pos[idx] = self.bit_pos[idx].wrapping_add(elt);
    }

    /// Port of `HUF_zeroIndex1` (external/zstd/lib/compress/huf_compress.c:911-915
    /// @ zstd v1.5.7).
    #[inline(always)]
    fn zero_index1(&mut self) {
        self.bit_container[1] = 0;
        self.bit_pos[1] = 0;
    }

    /// Port of `HUF_mergeIndex1` (external/zstd/lib/compress/huf_compress.c:921-928
    /// @ zstd v1.5.7).
    #[inline(always)]
    fn merge_index1(&mut self) {
        self.bit_container[0] >>= self.bit_pos[1] & 0xFF;
        self.bit_container[0] |= self.bit_container[1];
        self.bit_pos[0] = self.bit_pos[0].wrapping_add(self.bit_pos[1]);
    }

    /// Port of `HUF_flushBits` (external/zstd/lib/compress/huf_compress.c:937-957
    /// @ zstd v1.5.7): `K_FAST` knows the container can't overflow the buffer.
    #[inline(always)]
    fn flush_bits<const K_FAST: bool>(&mut self) {
        // The upper bits of bitPos are noisy, so we must mask by 0xFF.
        let nb_bits = self.bit_pos[0] & 0xFF;
        let nb_bytes = (nb_bits >> 3) as usize;
        // The top nbBits bits of bitContainer are the ones we need (`shr` takes the count
        // modulo 64, as x86-64 does).
        let bit_container = self.bit_container[0].wrapping_shr((64 - nb_bits) as u32);
        // Mask bitPos to account for the bytes we consumed.
        self.bit_pos[0] &= 7;
        self.dst[self.ptr..self.ptr + 8].copy_from_slice(&bit_container.to_le_bytes());
        self.ptr += nb_bytes;
        if !K_FAST && self.ptr > self.end_ptr {
            self.ptr = self.end_ptr;
        }
    }

    /// Port of `HUF_closeCStream` (external/zstd/lib/compress/huf_compress.c:973-982
    /// @ zstd v1.5.7): the stream's size, or 0 when it didn't fit.
    fn close(&mut self) -> usize {
        // Port of `HUF_endMark` (external/zstd/lib/compress/huf_compress.c:962-968
        // @ zstd v1.5.7): a 1-bit value = 1.
        let mut end_mark = 0;
        set_nb_bits(&mut end_mark, 1);
        set_value(&mut end_mark, 1);
        self.add_bits::<false>(end_mark, 0);
        self.flush_bits::<false>();
        let nb_bits = self.bit_pos[0] & 0xFF;
        if self.ptr >= self.end_ptr {
            return 0; // overflow detected
        }
        self.ptr + usize::from(nb_bits > 0)
    }
}

/// Port of `HUF_compress1X_usingCTable_internal_body_loop`
/// (external/zstd/lib/compress/huf_compress.c:990-1042 @ zstd v1.5.7): the symbols from the
/// last, `K_UNROLL` at a time into each container in turn.
#[inline(always)]
fn huf_compress1x_body_loop<
    const K_UNROLL: i32,
    const K_FAST_FLUSH: bool,
    const K_LAST_FAST: bool,
>(
    bit_c: &mut HufCStream<'_>,
    ip: &[u8],
    ct: &[u64],
) {
    let enc = |b: &mut HufCStream<'_>, i: i32, idx: usize, fast: bool| {
        let elt = ct[usize::from(ip[i as usize])];
        if fast {
            b.add_bits::<true>(elt, idx);
        } else {
            b.add_bits::<false>(elt, idx);
        }
    };
    // Join to kUnroll
    let mut n = ip.len() as i32;
    let mut rem = n % K_UNROLL;
    if rem > 0 {
        while rem > 0 {
            n -= 1;
            enc(bit_c, n, 0, false);
            rem -= 1;
        }
        bit_c.flush_bits::<K_FAST_FLUSH>();
    }
    // Join to 2 * kUnroll
    if n % (2 * K_UNROLL) != 0 {
        for u in 1..K_UNROLL {
            enc(bit_c, n - u, 0, true);
        }
        enc(bit_c, n - K_UNROLL, 0, K_LAST_FAST);
        bit_c.flush_bits::<K_FAST_FLUSH>();
        n -= K_UNROLL;
    }
    while n > 0 {
        // Encode kUnroll symbols into the bitstream @ index 0.
        for u in 1..K_UNROLL {
            enc(bit_c, n - u, 0, true);
        }
        enc(bit_c, n - K_UNROLL, 0, K_LAST_FAST);
        bit_c.flush_bits::<K_FAST_FLUSH>();
        // Encode kUnroll symbols into the bitstream @ index 1.
        bit_c.zero_index1();
        for u in 1..K_UNROLL {
            enc(bit_c, n - K_UNROLL - u, 1, true);
        }
        enc(bit_c, n - K_UNROLL - K_UNROLL, 1, K_LAST_FAST);
        // Merge bitstream @ index 1 into the bitstream @ index 0
        bit_c.merge_index1();
        bit_c.flush_bits::<K_FAST_FLUSH>();
        n -= 2 * K_UNROLL;
    }
}

/// Port of `HUF_tightCompressBound` (external/zstd/lib/compress/huf_compress.c:1049-1052
/// @ zstd v1.5.7).
#[inline]
fn huf_tight_compress_bound(src_size: usize, table_log: usize) -> usize {
    ((src_size * table_log) >> 3) + 8
}

/// Port of `HUF_compress1X_usingCTable_internal_body`
/// (external/zstd/lib/compress/huf_compress.c:1055-1118 @ zstd v1.5.7), on 64-bit: one
/// stream; 0 when it doesn't fit. The BMI2 build runs the same code.
#[inline(always)]
pub(crate) fn huf_compress1x_using_ctable_body(
    dst: &mut [u8],
    src: &[u8],
    ctable: &HufCTable,
) -> usize {
    let (table_log, _) = read_ctable_header(ctable);
    let ct = &ctable[1..];
    let dst_size = dst.len();

    // init
    if dst_size < 8 {
        return 0; // not enough space to compress
    }
    let Ok(mut bit_c) = HufCStream::init(dst) else {
        return 0;
    };

    if dst_size < huf_tight_compress_bound(src.len(), table_log as usize) || table_log > 11 {
        huf_compress1x_body_loop::<4, false, false>(&mut bit_c, src, ct);
    } else {
        match table_log {
            11 => huf_compress1x_body_loop::<5, true, false>(&mut bit_c, src, ct),
            10 => huf_compress1x_body_loop::<5, true, true>(&mut bit_c, src, ct),
            9 => huf_compress1x_body_loop::<6, true, false>(&mut bit_c, src, ct),
            8 => huf_compress1x_body_loop::<7, true, false>(&mut bit_c, src, ct),
            7 => huf_compress1x_body_loop::<8, true, false>(&mut bit_c, src, ct),
            _ => huf_compress1x_body_loop::<9, true, true>(&mut bit_c, src, ct),
        }
    }
    bit_c.close()
}

/// Port of `HUF_compress4X_usingCTable_internal`
/// (external/zstd/lib/compress/huf_compress.c:1167-1215 @ zstd v1.5.7): four streams behind a
/// jump table of their sizes; 0 when they don't fit.
fn huf_compress4x_using_ctable(dst: &mut [u8], src: &[u8], ctable: &HufCTable) -> usize {
    let src_size = src.len();
    let segment_size = src_size.div_ceil(4); // first 3 segments
    let dst_size = dst.len();
    let mut ip = 0;
    let mut op = 0;

    if dst_size < 6 + 1 + 1 + 1 + 8 {
        return 0; // minimum space to compress successfully
    }
    if src_size < 12 {
        return 0; // no saving possible : too small input
    }
    op += 6; // jumpTable

    for k in 0..4 {
        let seg = if k < 3 {
            &src[ip..ip + segment_size]
        } else {
            &src[ip..]
        };
        let c_size = huf_compress1x_using_ctable(&mut dst[op..], seg, ctable);
        if c_size == 0 || c_size > 65535 {
            return 0;
        }
        if k < 3 {
            dst[2 * k..2 * k + 2].copy_from_slice(&(c_size as u16).to_le_bytes());
        }
        op += c_size;
        ip += segment_size;
    }
    op
}

/// Port of `HUF_compressCTable_internal` (external/zstd/lib/compress/huf_compress.c:1224-1239
/// @ zstd v1.5.7): the streams after `op`, the table's description; returns the whole size,
/// or 0 when it doesn't save a byte.
fn huf_compress_ctable_internal(
    dst: &mut [u8],
    op: usize,
    src: &[u8],
    nb_streams: NbStreams,
    ctable: &HufCTable,
) -> usize {
    let c_size = match nb_streams {
        NbStreams::Single => huf_compress1x_using_ctable(&mut dst[op..], src, ctable),
        NbStreams::Four => huf_compress4x_using_ctable(&mut dst[op..], src, ctable),
    };
    if c_size == 0 {
        return 0; // uncompressible
    }
    let op = op + c_size;
    // check compressibility
    if op >= src.len() - 1 {
        return 0;
    }
    op
}

/// `SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE` (external/zstd/lib/compress/huf_compress.c:1251
/// @ zstd v1.5.7).
const SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE: usize = 4096;
/// `SUSPECT_INCOMPRESSIBLE_SAMPLE_RATIO` (external/zstd/lib/compress/huf_compress.c:1252
/// @ zstd v1.5.7).
const SUSPECT_INCOMPRESSIBLE_SAMPLE_RATIO: usize = 10;

/// Port of `HUF_cardinality` (external/zstd/lib/compress/huf_compress.c:1254-1264
/// @ zstd v1.5.7).
pub(crate) fn huf_cardinality(count: &[u32], max_symbol_value: u32) -> u32 {
    count[..=max_symbol_value as usize]
        .iter()
        .filter(|&&c| c != 0)
        .count() as u32
}

/// Port of `HUF_minTableLog` (external/zstd/lib/compress/huf_compress.c:1266-1270
/// @ zstd v1.5.7).
pub(crate) fn huf_min_table_log(symbol_cardinality: u32) -> u32 {
    highbit32(symbol_cardinality) + 1
}

/// Port of `HUF_optimalTableLog` (external/zstd/lib/compress/huf_compress.c:1272-1327
/// @ zstd v1.5.7): FSE's estimate, or with `HUF_flags_optimalDepth` the depth whose table and
/// stream are smallest. `table` is scratch.
pub(crate) fn huf_optimal_table_log(
    max_table_log: u32,
    src_size: usize,
    max_symbol_value: u32,
    table: &mut HufCTable,
    count: &[u32],
    flags: i32,
) -> u32 {
    if flags & HUF_FLAGS_OPTIMAL_DEPTH == 0 {
        // cheap evaluation, based on FSE
        return fse_optimal_table_log_internal(max_table_log, src_size, max_symbol_value, 1);
    }

    // The scratch upstream writes the descriptions into: the workspace past
    // `HUF_WriteCTableWksp` (4864 - 748 bytes).
    let mut dst = [0u8; 4864 - 748];
    let symbol_cardinality = huf_cardinality(count, max_symbol_value);
    let min_table_log = huf_min_table_log(symbol_cardinality);
    let mut opt_size: usize = (!0usize) - 1;
    let mut opt_log = max_table_log;

    // Search until size increases
    for opt_log_guess in min_table_log..=max_table_log {
        let h_size;
        {
            let Ok(max_bits) = huf_build_ctable(table, count, max_symbol_value, opt_log_guess)
            else {
                continue;
            };
            if max_bits < opt_log_guess && opt_log_guess > min_table_log {
                break;
            }
            match huf_write_ctable(&mut dst, table, max_symbol_value, max_bits) {
                Ok(s) => h_size = s,
                Err(_) => continue,
            }
        }
        let new_size = huf_estimate_compressed_size(table, count, max_symbol_value) + h_size;
        if new_size > opt_size.wrapping_add(1) {
            break;
        }
        if new_size < opt_size {
            opt_size = new_size;
            opt_log = opt_log_guess;
        }
    }
    opt_log
}

/// The persistent part of `HUF_compress_tables_t`
/// (external/zstd/lib/compress/huf_compress.c:1241-1249 @ zstd v1.5.7): the histogram and the table
/// being built.
pub(crate) struct HufCompressTables {
    pub(crate) count: [u32; HUF_SYMBOLVALUE_MAX as usize + 1],
    pub(crate) ctable: HufCTable,
}

impl HufCompressTables {
    pub(crate) fn new() -> Self {
        HufCompressTables {
            count: [0; HUF_SYMBOLVALUE_MAX as usize + 1],
            ctable: [0; HUF_CTABLE_SIZE_ST],
        }
    }
}

/// Port of `HUF_compress_internal` (external/zstd/lib/compress/huf_compress.c:1332-1434
/// @ zstd v1.5.7): `src` Huffman-coded into `dst`, a new table's description first unless the
/// old table `old_huf_table` is reused; 0 when not compressible, 1 (with the symbol in
/// `dst[0]`) when `src` is one symbol.
#[allow(clippy::too_many_arguments)]
fn huf_compress_internal(
    dst: &mut [u8],
    src: &[u8],
    max_symbol_value: u32,
    huff_log: u32,
    nb_streams: NbStreams,
    table: &mut HufCompressTables,
    old_huf_table: &mut HufCTable,
    repeat: &mut HufRepeat,
    flags: i32,
) -> ZResult<usize> {
    let dst_size = dst.len();
    let src_size = src.len();
    let mut max_symbol_value = max_symbol_value;
    let mut huff_log = huff_log;
    let mut op = 0;

    // checks & inits
    if src_size == 0 {
        return Ok(0); // Uncompressed
    }
    if dst_size == 0 {
        return Ok(0); // cannot fit anything within dst budget
    }
    if src_size > HUF_BLOCKSIZE_MAX {
        return Err(ErrorCode::SrcSizeWrong); // current block size limit
    }
    if huff_log > HUF_TABLELOG_MAX {
        return Err(ErrorCode::TableLogTooLarge);
    }
    if max_symbol_value > HUF_SYMBOLVALUE_MAX {
        return Err(ErrorCode::MaxSymbolValueTooLarge);
    }
    if max_symbol_value == 0 {
        max_symbol_value = HUF_SYMBOLVALUE_MAX;
    }
    if huff_log == 0 {
        huff_log = HUF_TABLELOG_DEFAULT;
    }

    // Heuristic : If old table is valid, use it for small inputs
    if (flags & HUF_FLAGS_PREFER_REPEAT != 0) && *repeat == HufRepeat::Valid {
        return Ok(huf_compress_ctable_internal(
            dst,
            op,
            src,
            nb_streams,
            old_huf_table,
        ));
    }

    // If uncompressible data is suspected, do a smaller sampling first
    if (flags & HUF_FLAGS_SUSPECT_UNCOMPRESSIBLE != 0)
        && src_size >= (SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE * SUSPECT_INCOMPRESSIBLE_SAMPLE_RATIO)
    {
        let mut largest_total: usize = 0;
        {
            let mut max_symbol_value_begin = max_symbol_value;
            let largest_begin = hist_count_simple(
                &mut table.count,
                &mut max_symbol_value_begin,
                &src[..SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE],
            );
            largest_total += largest_begin as usize;
        }
        {
            let mut max_symbol_value_end = max_symbol_value;
            let largest_end = hist_count_simple(
                &mut table.count,
                &mut max_symbol_value_end,
                &src[src_size - SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE..],
            );
            largest_total += largest_end as usize;
        }
        if largest_total <= ((2 * SUSPECT_INCOMPRESSIBLE_SAMPLE_SIZE) >> 7) + 4 {
            return Ok(0); // heuristic : probably not compressible enough
        }
    }

    // Scan input and build symbol stats
    {
        let largest = hist_count(&mut table.count, &mut max_symbol_value, src)? as usize;
        if largest == src_size {
            dst[0] = src[0];
            return Ok(1); // single symbol, rle
        }
        if largest <= (src_size >> 7) + 4 {
            return Ok(0); // heuristic : probably not compressible enough
        }
    }

    // Check validity of previous table
    if *repeat == HufRepeat::Check
        && !huf_validate_ctable(old_huf_table, &table.count, max_symbol_value)
    {
        *repeat = HufRepeat::None;
    }
    // Heuristic : use existing table for small inputs
    if (flags & HUF_FLAGS_PREFER_REPEAT != 0) && *repeat != HufRepeat::None {
        return Ok(huf_compress_ctable_internal(
            dst,
            op,
            src,
            nb_streams,
            old_huf_table,
        ));
    }

    // Build Huffman Tree
    huff_log = huf_optimal_table_log(
        huff_log,
        src_size,
        max_symbol_value,
        &mut table.ctable,
        &table.count,
        flags,
    );
    {
        let max_bits =
            huf_build_ctable(&mut table.ctable, &table.count, max_symbol_value, huff_log)?;
        huff_log = max_bits;
    }

    // Write table description header
    {
        let h_size = huf_write_ctable(dst, &table.ctable, max_symbol_value, huff_log)?;
        // Check if using previous huffman table is beneficial
        if *repeat != HufRepeat::None {
            let old_size =
                huf_estimate_compressed_size(old_huf_table, &table.count, max_symbol_value);
            let new_size =
                huf_estimate_compressed_size(&table.ctable, &table.count, max_symbol_value);
            if old_size <= h_size + new_size || h_size + 12 >= src_size {
                return Ok(huf_compress_ctable_internal(
                    dst,
                    op,
                    src,
                    nb_streams,
                    old_huf_table,
                ));
            }
        }

        // Use the new huffman table
        if h_size + 12 >= src_size {
            return Ok(0);
        }
        op += h_size;
        *repeat = HufRepeat::None;
        *old_huf_table = table.ctable; // Save new table
    }
    Ok(huf_compress_ctable_internal(
        dst,
        op,
        src,
        nb_streams,
        &table.ctable,
    ))
}

/// Port of `HUF_compress1X_repeat` (external/zstd/lib/compress/huf_compress.c:1436-1447
/// @ zstd v1.5.7).
#[allow(clippy::too_many_arguments)]
pub(crate) fn huf_compress1x_repeat(
    dst: &mut [u8],
    src: &[u8],
    max_symbol_value: u32,
    huff_log: u32,
    table: &mut HufCompressTables,
    huf_table: &mut HufCTable,
    repeat: &mut HufRepeat,
    flags: i32,
) -> ZResult<usize> {
    huf_compress_internal(
        dst,
        src,
        max_symbol_value,
        huff_log,
        NbStreams::Single,
        table,
        huf_table,
        repeat,
        flags,
    )
}

/// Port of `HUF_compress4X_repeat` (external/zstd/lib/compress/huf_compress.c:1453-1464
/// @ zstd v1.5.7).
#[allow(clippy::too_many_arguments)]
pub(crate) fn huf_compress4x_repeat(
    dst: &mut [u8],
    src: &[u8],
    max_symbol_value: u32,
    huff_log: u32,
    table: &mut HufCompressTables,
    huf_table: &mut HufCTable,
    repeat: &mut HufRepeat,
    flags: i32,
) -> ZResult<usize> {
    huf_compress_internal(
        dst,
        src,
        max_symbol_value,
        huff_log,
        NbStreams::Four,
        table,
        huf_table,
        repeat,
        flags,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huf_decompress::{HufDTable, huf_decompress_1x1_dctx, huf_decompress_4x_huf_only};
    use crate::zstd_decompress_internal::{HUF_DECOMPRESS_WORKSPACE_SIZE, HUFFDTABLE_CAPACITY_LOG};

    /// Bytes of a skewed distribution: `shape` picks how fast the counts fall.
    fn skewed(len: usize, shape: u32, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1664525).wrapping_add(1013904223);
                let r = x >> 8;
                (r.trailing_zeros().min(31) * shape + (r >> 28)) as u8
            })
            .collect()
    }

    /// A geometric histogram, symbol `k` twice as frequent as `k + 1`, deep enough that
    /// the tree must be limited to the table log; its order interleaved.
    fn geometric(depth: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for k in 0..depth {
            v.extend(std::iter::repeat_n(k as u8, 1usize << (depth - 1 - k)));
        }
        v.push(depth as u8);
        // 2^depth symbols, interleaved by an odd stride so any prefix has them all.
        (0..v.len()).map(|i| v[(i * 7919) % v.len()]).collect()
    }

    fn round_trip(src: &[u8], four: bool) -> usize {
        let mut table = HufCompressTables::new();
        let mut huf_table = [0u64; HUF_CTABLE_SIZE_ST];
        let mut repeat = HufRepeat::None;
        let mut dst = vec![0u8; huf_compress_bound(src.len())];
        let compress = if four {
            huf_compress4x_repeat
        } else {
            huf_compress1x_repeat
        };
        let c_size = compress(
            &mut dst,
            src,
            HUF_SYMBOLVALUE_MAX,
            HUF_TABLELOG_DEFAULT,
            &mut table,
            &mut huf_table,
            &mut repeat,
            0,
        )
        .unwrap();
        assert!(c_size > 1, "compressed to {c_size}");
        assert_eq!(repeat, HufRepeat::None);
        assert_eq!(huf_table, table.ctable);
        let mut dtable = HufDTable::new(HUFFDTABLE_CAPACITY_LOG);
        let mut out = vec![0u8; src.len()];
        let n = if four {
            huf_decompress_4x_huf_only(
                &mut dtable,
                &mut out,
                &dst[..c_size],
                HUF_DECOMPRESS_WORKSPACE_SIZE,
            )
        } else {
            huf_decompress_1x1_dctx(
                &mut dtable,
                &mut out,
                &dst[..c_size],
                HUF_DECOMPRESS_WORKSPACE_SIZE,
            )
        }
        .unwrap();
        assert_eq!(n, src.len());
        assert!(out == src);
        c_size
    }

    #[test]
    fn huffman_streams_decode_to_their_input() {
        for &len in &[40usize, 255, 256, 1000, 1024, 5000, 16384, 65536, 131072] {
            for shape in [1, 3, 9] {
                let src = skewed(len, shape, len as u32 ^ shape);
                if len < 1024 {
                    round_trip(&src, false);
                }
                if len >= 256 {
                    round_trip(&src, true);
                }
            }
        }
    }

    #[test]
    fn deep_trees_are_limited_to_the_table_log() {
        for depth in [13, 15, 17] {
            let src = geometric(depth);
            let mut ctable = [0u64; HUF_CTABLE_SIZE_ST];
            let mut count = [0u32; 256];
            let mut msv = 255;
            hist_count(&mut count, &mut msv, &src).unwrap();
            let max_bits =
                huf_build_ctable(&mut ctable, &count, msv, HUF_TABLELOG_DEFAULT).unwrap();
            assert_eq!(max_bits, HUF_TABLELOG_DEFAULT);
            // Kraft's sum of the limited tree is exactly one.
            let kraft: u64 = (0..=msv as usize)
                .map(|s| get_nb_bits(ctable[s + 1]))
                .filter(|&b| b != 0)
                .map(|b| 1u64 << (max_bits as u64 - b))
                .sum();
            assert_eq!(kraft, 1u64 << max_bits);
            round_trip(&src, true);
            round_trip(&src[..1000.min(src.len())], false);
        }
    }

    #[test]
    fn one_symbol_and_flat_inputs_are_not_coded() {
        let mut table = HufCompressTables::new();
        let mut huf_table = [0u64; HUF_CTABLE_SIZE_ST];
        let mut repeat = HufRepeat::None;
        let mut dst = vec![0u8; 1024];
        let one = [7u8; 300];
        let r = huf_compress4x_repeat(
            &mut dst,
            &one,
            255,
            11,
            &mut table,
            &mut huf_table,
            &mut repeat,
            0,
        );
        assert_eq!(r, Ok(1));
        assert_eq!(dst[0], 7);
        let flat: Vec<u8> = (0..512u32).map(|i| i as u8).collect();
        let r = huf_compress4x_repeat(
            &mut dst,
            &flat,
            255,
            11,
            &mut table,
            &mut huf_table,
            &mut repeat,
            0,
        );
        assert_eq!(r, Ok(0));
    }
}
