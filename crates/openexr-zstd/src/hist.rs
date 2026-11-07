// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `hist.c` (external/zstd/lib/compress/hist.c:1-191 @ zstd v1.5.7): byte histograms.
//! The four-way counting of large inputs is a speed device; its counts are the simple loop's.

use crate::error_private::{ErrorCode, ZResult};

/// Port of `HIST_add` (external/zstd/lib/compress/hist.c:23-31 @ zstd v1.5.7).
pub(crate) fn hist_add(count: &mut [u32], src: &[u8]) {
    for &b in src {
        count[usize::from(b)] += 1;
    }
}

/// Port of `HIST_count_simple` (external/zstd/lib/compress/hist.c:39-63 @ zstd v1.5.7): the
/// counts of `src`, whose symbols are at most `*max_symbol_value`, which becomes the largest
/// present; returns the largest count.
pub(crate) fn hist_count_simple(count: &mut [u32], max_symbol_value: &mut u32, src: &[u8]) -> u32 {
    let mut msv = *max_symbol_value;
    count[..=msv as usize].fill(0);
    if src.is_empty() {
        *max_symbol_value = 0;
        return 0;
    }
    for &b in src {
        count[usize::from(b)] += 1;
    }
    while count[msv as usize] == 0 {
        msv -= 1;
    }
    *max_symbol_value = msv;
    let mut largest_count = 0;
    for &c in &count[..=msv as usize] {
        if c > largest_count {
            largest_count = c;
        }
    }
    largest_count
}

/// Port of `HIST_count_parallel_wksp` (external/zstd/lib/compress/hist.c:77-142 @ zstd v1.5.7):
/// `check` refuses a symbol above `*max_symbol_value`.
fn hist_count_parallel(
    count: &mut [u32],
    max_symbol_value: &mut u32,
    src: &[u8],
    check: bool,
) -> ZResult<u32> {
    if src.is_empty() {
        count[..=*max_symbol_value as usize].fill(0);
        *max_symbol_value = 0;
        return Ok(0);
    }
    let mut counting = [0u32; 256];
    for &b in src {
        counting[usize::from(b)] += 1;
    }
    let mut max = 0;
    for &c in &counting {
        if c > max {
            max = c;
        }
    }
    let mut msv = 255usize;
    while counting[msv] == 0 {
        msv -= 1;
    }
    if check && msv as u32 > *max_symbol_value {
        return Err(ErrorCode::MaxSymbolValueTooSmall);
    }
    let count_size = *max_symbol_value as usize + 1;
    *max_symbol_value = msv as u32;
    count[..count_size].copy_from_slice(&counting[..count_size]);
    Ok(max)
}

/// Port of `HIST_countFast_wksp` (external/zstd/lib/compress/hist.c:149-158 @ zstd v1.5.7).
pub(crate) fn hist_count_fast(
    count: &mut [u32],
    max_symbol_value: &mut u32,
    src: &[u8],
) -> ZResult<u32> {
    if src.len() < 1500 {
        // heuristic threshold
        return Ok(hist_count_simple(count, max_symbol_value, src));
    }
    hist_count_parallel(count, max_symbol_value, src, false)
}

/// Port of `HIST_count_wksp` (external/zstd/lib/compress/hist.c:163-173 @ zstd v1.5.7).
pub(crate) fn hist_count(
    count: &mut [u32],
    max_symbol_value: &mut u32,
    src: &[u8],
) -> ZResult<u32> {
    if *max_symbol_value < 255 {
        return hist_count_parallel(count, max_symbol_value, src, true);
    }
    *max_symbol_value = 255;
    hist_count_fast(count, max_symbol_value, src)
}
