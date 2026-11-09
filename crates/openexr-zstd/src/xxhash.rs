// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of XXH64 from `xxhash.h` (external/zstd/lib/common/xxhash.h:3361-3516 @ zstd v1.5.7),
//! the frame checksum, in one pass over the data (the streaming state zstd keeps gives the same
//! digest over the same bytes).

const PRIME64_1: u64 = 0x9E37_79B1_85EB_CA87;
const PRIME64_2: u64 = 0xC2B2_AE3D_27D4_EB4F;
const PRIME64_3: u64 = 0x1656_67B1_9E37_79F9;
const PRIME64_4: u64 = 0x85EB_CA77_C2B2_AE63;
const PRIME64_5: u64 = 0x27D4_EB2F_1656_67C5;

fn read64(p: &[u8]) -> u64 {
    u64::from_le_bytes(p[..8].try_into().unwrap())
}

/// Port of `XXH64_round` (external/zstd/lib/common/xxhash.h:3376-3399 @ zstd v1.5.7).
fn round(mut acc: u64, input: u64) -> u64 {
    acc = acc.wrapping_add(input.wrapping_mul(PRIME64_2));
    acc = acc.rotate_left(31);
    acc.wrapping_mul(PRIME64_1)
}

/// Port of `XXH64_mergeRound` (external/zstd/lib/common/xxhash.h:3401-3407 @ zstd v1.5.7).
fn merge_round(mut acc: u64, val: u64) -> u64 {
    let val = round(0, val);
    acc ^= val;
    acc.wrapping_mul(PRIME64_1).wrapping_add(PRIME64_4)
}

/// Port of `XXH64_avalanche` (external/zstd/lib/common/xxhash.h:3410-3418 @ zstd v1.5.7).
fn avalanche(mut hash: u64) -> u64 {
    hash ^= hash >> 33;
    hash = hash.wrapping_mul(PRIME64_2);
    hash ^= hash >> 29;
    hash = hash.wrapping_mul(PRIME64_3);
    hash ^= hash >> 32;
    hash
}

/// Port of `XXH64_finalize` (external/zstd/lib/common/xxhash.h:3438-3464 @ zstd v1.5.7).
fn finalize(mut hash: u64, mut p: &[u8]) -> u64 {
    while p.len() >= 8 {
        let k1 = round(0, read64(p));
        p = &p[8..];
        hash ^= k1;
        hash = hash
            .rotate_left(27)
            .wrapping_mul(PRIME64_1)
            .wrapping_add(PRIME64_4);
    }
    if p.len() >= 4 {
        hash ^= u64::from(u32::from_le_bytes(p[..4].try_into().unwrap())).wrapping_mul(PRIME64_1);
        p = &p[4..];
        hash = hash
            .rotate_left(23)
            .wrapping_mul(PRIME64_2)
            .wrapping_add(PRIME64_3);
    }
    for &b in p {
        hash ^= u64::from(b).wrapping_mul(PRIME64_5);
        hash = hash.rotate_left(11).wrapping_mul(PRIME64_1);
    }
    avalanche(hash)
}

/// Port of `XXH64` and `XXH64_endian_align` (external/zstd/lib/common/xxhash.h:3482-3516
/// @ zstd v1.5.7).
pub(crate) fn xxh64(input: &[u8], seed: u64) -> u64 {
    let len = input.len();
    let mut p = input;
    let mut h64: u64;
    if len >= 32 {
        let mut v1 = seed.wrapping_add(PRIME64_1).wrapping_add(PRIME64_2);
        let mut v2 = seed.wrapping_add(PRIME64_2);
        let mut v3 = seed;
        let mut v4 = seed.wrapping_sub(PRIME64_1);
        while p.len() >= 32 {
            v1 = round(v1, read64(p));
            v2 = round(v2, read64(&p[8..]));
            v3 = round(v3, read64(&p[16..]));
            v4 = round(v4, read64(&p[24..]));
            p = &p[32..];
        }
        h64 = v1
            .rotate_left(1)
            .wrapping_add(v2.rotate_left(7))
            .wrapping_add(v3.rotate_left(12))
            .wrapping_add(v4.rotate_left(18));
        h64 = merge_round(h64, v1);
        h64 = merge_round(h64, v2);
        h64 = merge_round(h64, v3);
        h64 = merge_round(h64, v4);
    } else {
        h64 = seed.wrapping_add(PRIME64_5);
    }
    h64 = h64.wrapping_add(len as u64);
    finalize(h64, p)
}
