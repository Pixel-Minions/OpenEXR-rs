// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `matchfinder_common.h` (external/deflate/lib/matchfinder_common.h:1-224
//! @ libdeflate v1.26): what the Lempel-Ziv matchfinders share, for x86-64 (unaligned word
//! access, little-endian).
//!
//! Positions are `i16`s relative to a base that slides forward a window at a time; the input
//! is one slice and every pointer upstream keeps into it is an index here. The x86 builds of
//! `matchfinder_init` and `matchfinder_rebase` (`x86/matchfinder_impl.h`, SSE2 and AVX2)
//! store the same values as the portable loops ported here.

use crate::deflate_constants::DEFLATE_WINDOW_ORDER;

/// `MATCHFINDER_WINDOW_ORDER`, as `deflate_compress.c` defines it: the DEFLATE window.
pub(crate) const MATCHFINDER_WINDOW_ORDER: u32 = DEFLATE_WINDOW_ORDER;

/// Port of `MATCHFINDER_WINDOW_SIZE` (external/deflate/lib/matchfinder_common.h:47
/// @ libdeflate v1.26).
pub(crate) const MATCHFINDER_WINDOW_SIZE: u32 = 1 << MATCHFINDER_WINDOW_ORDER;

/// Port of `mf_pos_t` (external/deflate/lib/matchfinder_common.h:49 @ libdeflate v1.26): a
/// position relative to the matchfinder's base.
pub(crate) type MfPos = i16;

/// Port of `MATCHFINDER_INITVAL` (external/deflate/lib/matchfinder_common.h:51 @ libdeflate
/// v1.26): a position outside every window.
pub(crate) const MATCHFINDER_INITVAL: MfPos = -(MATCHFINDER_WINDOW_SIZE as i32) as MfPos;

/// Port of `matchfinder_init` (external/deflate/lib/matchfinder_common.h:107-115 @ libdeflate
/// v1.26): every entry outside the window.
pub(crate) fn matchfinder_init(data: &mut [MfPos]) {
    data.fill(MATCHFINDER_INITVAL);
}

/// Port of `matchfinder_rebase` (external/deflate/lib/matchfinder_common.h:136-158
/// @ libdeflate v1.26): slides every entry back by a window, those already a window back
/// becoming [`MATCHFINDER_INITVAL`].
pub(crate) fn matchfinder_rebase(data: &mut [MfPos]) {
    // MATCHFINDER_WINDOW_SIZE == 32768
    for d in data {
        let v = i32::from(*d);
        *d = (0x8000 | (v & !(v >> 15))) as MfPos;
    }
}

/// Port of `lz_hash` (external/deflate/lib/matchfinder_common.h:168-172 @ libdeflate v1.26).
#[inline]
pub(crate) fn lz_hash(seq: u32, num_bits: u32) -> u32 {
    seq.wrapping_mul(0x1E35A7BD) >> (32 - num_bits)
}

/// `load_u32_unaligned` on little-endian x86-64: the 4 bytes at `p`.
#[inline]
pub(crate) fn load_u32(data: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(data[p..p + 4].try_into().unwrap())
}

/// Port of `load_u24_unaligned` (external/deflate/lib/matchfinder_common.h:34-45 @ libdeflate
/// v1.26): the 3 bytes at `p`.
#[inline]
pub(crate) fn load_u24(data: &[u8], p: usize) -> u32 {
    loaded_u32_to_u24(load_u32(data, p))
}

/// Port of `loaded_u32_to_u24` (external/deflate/lib/matchfinder_common.h:20-27 @ libdeflate
/// v1.26), little-endian.
#[inline]
pub(crate) fn loaded_u32_to_u24(v: u32) -> u32 {
    v & 0xFFFFFF
}

/// The 8 bytes at `p`, little-endian (`load_word_unaligned`).
#[inline]
fn load_word(data: &[u8], p: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&data[p..p + 8]);
    u64::from_le_bytes(b)
}

/// Port of `lz_extend` (external/deflate/lib/matchfinder_common.h:178-222 @ libdeflate v1.26):
/// how far the match at `matchptr` agrees with the string at `strptr`, from `start_len` up to
/// `max_len`, a word at a time.
#[inline]
pub(crate) fn lz_extend(
    data: &[u8],
    strptr: usize,
    matchptr: usize,
    start_len: u32,
    max_len: u32,
) -> u32 {
    let mut len = start_len as usize;
    let max_len = max_len as usize;
    // (upstream unrolls the first four word compares when 4 words remain; the bytes compared
    // and the length found are the same)
    while len + 8 <= max_len {
        let v_word = load_word(data, matchptr + len) ^ load_word(data, strptr + len);
        if v_word != 0 {
            return (len + (v_word.trailing_zeros() >> 3) as usize) as u32;
        }
        len += 8;
    }
    while len < max_len && data[matchptr + len] == data[strptr + len] {
        len += 1;
    }
    len as u32
}
