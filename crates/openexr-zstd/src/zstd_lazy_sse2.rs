// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `ZSTD_row_getSSEMask` (external/zstd/lib/compress/zstd_lazy.c:986-1004
//! @ zstd v1.5.7), the row match finder's tag comparison on x86-64, where upstream defines
//! `ZSTD_ARCH_X86_SSE2` (external/zstd/lib/common/compiler.h:214-217): both wheels' builds.
//! SSE2 is baseline on x86-64.

#![allow(unsafe_code)]

use std::arch::x86_64::{
    __m128i, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
};

/// The row's match mask, `nb_chunks` (1, 2 or 4) chunks of 16 tags from `src`, rotated right
/// by `head` within the row: bit `n` set when the `n`th entry after the head has `tag`.
#[inline]
pub(crate) fn row_get_sse_mask(nb_chunks: usize, src: &[u8], tag: u8, head: u32) -> u64 {
    debug_assert!(nb_chunks == 1 || nb_chunks == 2 || nb_chunks == 4);
    let row = &src[..16 * nb_chunks];
    let mut matches = [0u32; 4];
    // SAFETY: SSE2, which these intrinsics need, is baseline on x86-64; each load is an
    // unaligned 16 bytes from a slice that holds them.
    unsafe {
        let comparison_mask = _mm_set1_epi8(tag as i8);
        for (i, m) in matches.iter_mut().enumerate().take(nb_chunks) {
            let chunk16 = &row[16 * i..16 * i + 16];
            let chunk = _mm_loadu_si128(chunk16.as_ptr().cast::<__m128i>());
            let equal_mask = _mm_cmpeq_epi8(chunk, comparison_mask);
            *m = _mm_movemask_epi8(equal_mask) as u32;
        }
    }
    match nb_chunks {
        1 => u64::from((matches[0] as u16).rotate_right(head)),
        2 => u64::from(((matches[1] << 16) | matches[0]).rotate_right(head)),
        _ => ((u64::from(matches[3]) << 48)
            | (u64::from(matches[2]) << 32)
            | (u64::from(matches[1]) << 16)
            | u64::from(matches[0]))
        .rotate_right(head),
    }
}
