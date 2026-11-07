// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `adler32.c` (external/deflate/lib/adler32.c:1-162 @ libdeflate v1.26): the Adler-32
//! checksum, its portable `adler32_generic` (the x86 vector versions give the same sums).

/// The Adler-32 divisor, or "base", value
const DIVISOR: u32 = 65521;

/// The most bytes that can be processed without the possibility of s2 overflowing when it is
/// represented as an unsigned 32-bit integer.
const MAX_CHUNK_LEN: usize = 5552;

/// Port of `ADLER32_CHUNK` (external/deflate/lib/adler32.c:75-103 @ libdeflate v1.26): updates
/// s1 and s2 with `p` and reduces them mod DIVISOR, four bytes at a time first.
fn adler32_chunk(s1: &mut u32, s2: &mut u32, mut p: &[u8]) {
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
/// updates the running Adler-32 `adler` with `buffer`; `None` gives the initial value, 1.
pub fn adler32(adler: u32, buffer: Option<&[u8]>) -> u32 {
    match buffer {
        None => 1,
        Some(b) => adler32_generic(adler, b),
    }
}
