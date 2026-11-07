// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `clevels.h` (external/zstd/lib/compress/clevels.h:1-134 @ zstd v1.5.7): the
//! compression parameters of each level, for inputs over 256 KB, up to 256 KB, up to 128 KB
//! and up to 16 KB.

use crate::zstd_compress_internal::{
    CParams, Strategy, ZSTD_BTLAZY2, ZSTD_BTOPT, ZSTD_BTULTRA, ZSTD_BTULTRA2, ZSTD_DFAST,
    ZSTD_FAST, ZSTD_GREEDY, ZSTD_LAZY, ZSTD_LAZY2,
};

/// `ZSTD_MAX_CLEVEL` (external/zstd/lib/compress/clevels.h:19 @ zstd v1.5.7).
pub(crate) const ZSTD_MAX_CLEVEL: i32 = 22;

const fn cp(w: u32, c: u32, h: u32, s: u32, l: u32, t: u32, strategy: Strategy) -> CParams {
    CParams {
        window_log: w,
        chain_log: c,
        hash_log: h,
        search_log: s,
        min_match: l,
        target_length: t,
        strategy,
    }
}

/// `ZSTD_defaultCParameters` (external/zstd/lib/compress/clevels.h:25-130 @ zstd v1.5.7):
/// window, chain, hash and search logs, minimum match, target length and strategy.
#[rustfmt::skip]
pub(crate) const ZSTD_DEFAULT_CPARAMETERS: [[CParams; ZSTD_MAX_CLEVEL as usize + 1]; 4] = [
    [
        cp(19, 12, 13, 1, 6, 1, ZSTD_FAST),
        cp(19, 13, 14, 1, 7, 0, ZSTD_FAST),
        cp(20, 15, 16, 1, 6, 0, ZSTD_FAST),
        cp(21, 16, 17, 1, 5, 0, ZSTD_DFAST),
        cp(21, 18, 18, 1, 5, 0, ZSTD_DFAST),
        cp(21, 18, 19, 3, 5, 2, ZSTD_GREEDY),
        cp(21, 18, 19, 3, 5, 4, ZSTD_LAZY),
        cp(21, 19, 20, 4, 5, 8, ZSTD_LAZY),
        cp(21, 19, 20, 4, 5, 16, ZSTD_LAZY2),
        cp(22, 20, 21, 4, 5, 16, ZSTD_LAZY2),
        cp(22, 21, 22, 5, 5, 16, ZSTD_LAZY2),
        cp(22, 21, 22, 6, 5, 16, ZSTD_LAZY2),
        cp(22, 22, 23, 6, 5, 32, ZSTD_LAZY2),
        cp(22, 22, 22, 4, 5, 32, ZSTD_BTLAZY2),
        cp(22, 22, 23, 5, 5, 32, ZSTD_BTLAZY2),
        cp(22, 23, 23, 6, 5, 32, ZSTD_BTLAZY2),
        cp(22, 22, 22, 5, 5, 48, ZSTD_BTOPT),
        cp(23, 23, 22, 5, 4, 64, ZSTD_BTOPT),
        cp(23, 23, 22, 6, 3, 64, ZSTD_BTULTRA),
        cp(23, 24, 22, 7, 3, 256, ZSTD_BTULTRA2),
        cp(25, 25, 23, 7, 3, 256, ZSTD_BTULTRA2),
        cp(26, 26, 24, 7, 3, 512, ZSTD_BTULTRA2),
        cp(27, 27, 25, 9, 3, 999, ZSTD_BTULTRA2),
    ],
    [
        cp(18, 12, 13, 1, 5, 1, ZSTD_FAST),
        cp(18, 13, 14, 1, 6, 0, ZSTD_FAST),
        cp(18, 14, 14, 1, 5, 0, ZSTD_DFAST),
        cp(18, 16, 16, 1, 4, 0, ZSTD_DFAST),
        cp(18, 16, 17, 3, 5, 2, ZSTD_GREEDY),
        cp(18, 17, 18, 5, 5, 2, ZSTD_GREEDY),
        cp(18, 18, 19, 3, 5, 4, ZSTD_LAZY),
        cp(18, 18, 19, 4, 4, 4, ZSTD_LAZY),
        cp(18, 18, 19, 4, 4, 8, ZSTD_LAZY2),
        cp(18, 18, 19, 5, 4, 8, ZSTD_LAZY2),
        cp(18, 18, 19, 6, 4, 8, ZSTD_LAZY2),
        cp(18, 18, 19, 5, 4, 12, ZSTD_BTLAZY2),
        cp(18, 19, 19, 7, 4, 12, ZSTD_BTLAZY2),
        cp(18, 18, 19, 4, 4, 16, ZSTD_BTOPT),
        cp(18, 18, 19, 4, 3, 32, ZSTD_BTOPT),
        cp(18, 18, 19, 6, 3, 128, ZSTD_BTOPT),
        cp(18, 19, 19, 6, 3, 128, ZSTD_BTULTRA),
        cp(18, 19, 19, 8, 3, 256, ZSTD_BTULTRA),
        cp(18, 19, 19, 6, 3, 128, ZSTD_BTULTRA2),
        cp(18, 19, 19, 8, 3, 256, ZSTD_BTULTRA2),
        cp(18, 19, 19, 10, 3, 512, ZSTD_BTULTRA2),
        cp(18, 19, 19, 12, 3, 512, ZSTD_BTULTRA2),
        cp(18, 19, 19, 13, 3, 999, ZSTD_BTULTRA2),
    ],
    [
        cp(17, 12, 12, 1, 5, 1, ZSTD_FAST),
        cp(17, 12, 13, 1, 6, 0, ZSTD_FAST),
        cp(17, 13, 15, 1, 5, 0, ZSTD_FAST),
        cp(17, 15, 16, 2, 5, 0, ZSTD_DFAST),
        cp(17, 17, 17, 2, 4, 0, ZSTD_DFAST),
        cp(17, 16, 17, 3, 4, 2, ZSTD_GREEDY),
        cp(17, 16, 17, 3, 4, 4, ZSTD_LAZY),
        cp(17, 16, 17, 3, 4, 8, ZSTD_LAZY2),
        cp(17, 16, 17, 4, 4, 8, ZSTD_LAZY2),
        cp(17, 16, 17, 5, 4, 8, ZSTD_LAZY2),
        cp(17, 16, 17, 6, 4, 8, ZSTD_LAZY2),
        cp(17, 17, 17, 5, 4, 8, ZSTD_BTLAZY2),
        cp(17, 18, 17, 7, 4, 12, ZSTD_BTLAZY2),
        cp(17, 18, 17, 3, 4, 12, ZSTD_BTOPT),
        cp(17, 18, 17, 4, 3, 32, ZSTD_BTOPT),
        cp(17, 18, 17, 6, 3, 256, ZSTD_BTOPT),
        cp(17, 18, 17, 6, 3, 128, ZSTD_BTULTRA),
        cp(17, 18, 17, 8, 3, 256, ZSTD_BTULTRA),
        cp(17, 18, 17, 10, 3, 512, ZSTD_BTULTRA),
        cp(17, 18, 17, 5, 3, 256, ZSTD_BTULTRA2),
        cp(17, 18, 17, 7, 3, 512, ZSTD_BTULTRA2),
        cp(17, 18, 17, 9, 3, 512, ZSTD_BTULTRA2),
        cp(17, 18, 17, 11, 3, 999, ZSTD_BTULTRA2),
    ],
    [
        cp(14, 12, 13, 1, 5, 1, ZSTD_FAST),
        cp(14, 14, 15, 1, 5, 0, ZSTD_FAST),
        cp(14, 14, 15, 1, 4, 0, ZSTD_FAST),
        cp(14, 14, 15, 2, 4, 0, ZSTD_DFAST),
        cp(14, 14, 14, 4, 4, 2, ZSTD_GREEDY),
        cp(14, 14, 14, 3, 4, 4, ZSTD_LAZY),
        cp(14, 14, 14, 4, 4, 8, ZSTD_LAZY2),
        cp(14, 14, 14, 6, 4, 8, ZSTD_LAZY2),
        cp(14, 14, 14, 8, 4, 8, ZSTD_LAZY2),
        cp(14, 15, 14, 5, 4, 8, ZSTD_BTLAZY2),
        cp(14, 15, 14, 9, 4, 8, ZSTD_BTLAZY2),
        cp(14, 15, 14, 3, 4, 12, ZSTD_BTOPT),
        cp(14, 15, 14, 4, 3, 24, ZSTD_BTOPT),
        cp(14, 15, 14, 5, 3, 32, ZSTD_BTULTRA),
        cp(14, 15, 15, 6, 3, 64, ZSTD_BTULTRA),
        cp(14, 15, 15, 7, 3, 256, ZSTD_BTULTRA),
        cp(14, 15, 15, 5, 3, 48, ZSTD_BTULTRA2),
        cp(14, 15, 15, 6, 3, 128, ZSTD_BTULTRA2),
        cp(14, 15, 15, 7, 3, 256, ZSTD_BTULTRA2),
        cp(14, 15, 15, 8, 3, 256, ZSTD_BTULTRA2),
        cp(14, 15, 15, 8, 3, 512, ZSTD_BTULTRA2),
        cp(14, 15, 15, 9, 3, 512, ZSTD_BTULTRA2),
        cp(14, 15, 15, 10, 3, 999, ZSTD_BTULTRA2),
    ],
];
