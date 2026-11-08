// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `adler32.c` (external/deflate/lib/adler32.c:1-162 @ libdeflate v1.26): the Adler-32
//! checksum, its portable `adler32_generic` (the x86 vector versions give the same sums).

/// The Adler-32 divisor, or "base", value
const DIVISOR: u32 = 65521;

/// The most bytes that can be processed without the possibility of s2 overflowing when it is
/// represented as an unsigned 32-bit integer.
pub(crate) const MAX_CHUNK_LEN: usize = 5552;

/// Port of `ADLER32_CHUNK` (external/deflate/lib/adler32.c:75-103 @ libdeflate v1.26): updates
/// s1 and s2 with `p` and reduces them mod DIVISOR, four bytes at a time first.
///
/// Sixteen bytes at a time come before the four: the same sums as the macro's, grouped in
/// sixteen lanes as its four-byte loop groups them in four, and as libdeflate's vector
/// versions group them. Over `n` blocks, s2 gains `16 * s1` before each block and each byte
/// weighted by its distance from its block's end; with `lanes[k]` the sum of the blocks'
/// `k`th bytes and `prev[k]` the sum, over the blocks, of `lanes[k]` before each, that is
/// `16 * (n * s1 + sum(prev)) + sum((16 - k) * lanes[k])`, and s1 gains `sum(lanes)`. No
/// lane is summed across until the end, so the loop vectorizes. Sums of products modulo 2^32
/// are exact here: `MAX_CHUNK_LEN` keeps the true s2 below 2^32, which is what lets the
/// macro's own grouping wrap.
pub(crate) fn adler32_chunk(s1: &mut u32, s2: &mut u32, mut p: &[u8]) {
    if p.len() >= 16 {
        let mut lanes = [0u32; 16];
        let mut prev = [0u32; 16];
        let (blocks, rest) = p.as_chunks::<16>();
        for b in blocks {
            for k in 0..16 {
                prev[k] = prev[k].wrapping_add(lanes[k]);
                lanes[k] = lanes[k].wrapping_add(u32::from(b[k]));
            }
        }
        let n = blocks.len() as u32;
        let mut before = n.wrapping_mul(*s1);
        let mut weighted: u32 = 0;
        let mut total: u32 = 0;
        for k in 0..16 {
            before = before.wrapping_add(prev[k]);
            weighted = weighted.wrapping_add((16 - k as u32).wrapping_mul(lanes[k]));
            total = total.wrapping_add(lanes[k]);
        }
        *s2 = s2
            .wrapping_add(16u32.wrapping_mul(before))
            .wrapping_add(weighted);
        *s1 = s1.wrapping_add(total);
        p = rest;
    }
    if p.len() >= 4 {
        let mut s1_sum: u32 = 0;
        let mut byte_0_sum: u32 = 0;
        let mut byte_1_sum: u32 = 0;
        let mut byte_2_sum: u32 = 0;
        let mut byte_3_sum: u32 = 0;
        while p.len() >= 4 {
            s1_sum = s1_sum.wrapping_add(*s1);
            *s1 = s1.wrapping_add(
                u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]) + u32::from(p[3]),
            );
            byte_0_sum = byte_0_sum.wrapping_add(u32::from(p[0]));
            byte_1_sum = byte_1_sum.wrapping_add(u32::from(p[1]));
            byte_2_sum = byte_2_sum.wrapping_add(u32::from(p[2]));
            byte_3_sum = byte_3_sum.wrapping_add(u32::from(p[3]));
            p = &p[4..];
        }
        *s2 = s2
            .wrapping_add(4u32.wrapping_mul(s1_sum.wrapping_add(byte_0_sum)))
            .wrapping_add(3u32.wrapping_mul(byte_1_sum))
            .wrapping_add(2u32.wrapping_mul(byte_2_sum))
            .wrapping_add(byte_3_sum);
    }
    for &b in p {
        *s1 = s1.wrapping_add(u32::from(b));
        *s2 = s2.wrapping_add(*s1);
    }
    *s1 %= DIVISOR;
    *s2 %= DIVISOR;
}

/// Port of `adler32_generic` (external/deflate/lib/adler32.c:105-119 @ libdeflate v1.26).
#[cfg_attr(target_arch = "x86_64", allow(dead_code))]
fn adler32_generic(adler: u32, mut p: &[u8]) -> u32 {
    let mut s1 = adler & 0xFFFF;
    let mut s2 = adler >> 16;
    while !p.is_empty() {
        let n = p.len().min(MAX_CHUNK_LEN & !3);
        adler32_chunk(&mut s1, &mut s2, &p[..n]);
        p = &p[n..];
    }
    (s2 << 16) | s1
}

/// Port of `libdeflate_adler32` (external/deflate/lib/adler32.c:156-162 @ libdeflate v1.26):
/// updates the running Adler-32 `adler` with `buffer`; `None` gives the initial value, 1. On
/// x86-64, by the vector variant `arch_select_adler32_func` chooses (`adler32_x86`), as
/// upstream's dispatch does; elsewhere `adler32_generic`.
pub fn adler32(adler: u32, buffer: Option<&[u8]>) -> u32 {
    match buffer {
        None => 1,
        #[cfg(target_arch = "x86_64")]
        Some(b) => crate::adler32_x86::adler32(adler, b),
        #[cfg(not(target_arch = "x86_64"))]
        Some(b) => adler32_generic(adler, b),
    }
}
