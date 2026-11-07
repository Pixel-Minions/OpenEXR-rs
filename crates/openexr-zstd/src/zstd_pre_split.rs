// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_preSplit.c` (external/zstd/lib/compress/zstd_preSplit.c:1-238 @ zstd v1.5.7):
//! where to end a full block, from how its byte statistics change. Upstream's workspace
//! overlays the middle fingerprint on the others' unused cells; separate tables read the same.

use crate::hist::hist_add;

const THRESHOLD_PENALTY_RATE: u64 = 16;
const THRESHOLD_BASE: u64 = THRESHOLD_PENALTY_RATE - 2;
const THRESHOLD_PENALTY: i32 = 3;
const HASHLENGTH: usize = 2;
const HASHLOG_MAX: u32 = 10;
const HASHTABLESIZE: usize = 1 << HASHLOG_MAX;
const KNUTH: u32 = 0x9e3779b9;
const CHUNKSIZE: usize = 8 << 10;
const SEGMENT_SIZE: usize = 512;

/// Port of `hash2` (external/zstd/lib/compress/zstd_preSplit.c:33-37 @ zstd v1.5.7).
#[inline]
fn hash2(p: &[u8], hash_log: u32) -> usize {
    if hash_log == 8 {
        return usize::from(p[0]);
    }
    (u32::from(u16::from_le_bytes([p[0], p[1]])).wrapping_mul(KNUTH) >> (32 - hash_log)) as usize
}

/// Port of `Fingerprint` (external/zstd/lib/compress/zstd_preSplit.c:39-42 @ zstd v1.5.7).
#[derive(Clone)]
struct Fingerprint {
    events: [u32; HASHTABLESIZE],
    nb_events: usize,
}

impl Fingerprint {
    fn new() -> Self {
        Fingerprint {
            events: [0; HASHTABLESIZE],
            nb_events: 0,
        }
    }
}

/// Port of `recordFingerprint_generic` (external/zstd/lib/compress/zstd_preSplit.c:66-73
/// @ zstd v1.5.7), with `addEvents_generic` (lines 54-64).
fn record_fingerprint(fp: &mut Fingerprint, src: &[u8], sampling_rate: usize, hash_log: u32) {
    fp.events[..1 << hash_log].fill(0);
    fp.nb_events = 0;
    let limit = src.len() - HASHLENGTH + 1;
    let mut n = 0;
    while n < limit {
        let h = hash2(&src[n..], hash_log);
        fp.events[h] = fp.events[h].wrapping_add(1);
        n += sampling_rate;
    }
    fp.nb_events += limit / sampling_rate;
}

/// Port of `abs64` (external/zstd/lib/compress/zstd_preSplit.c:92 @ zstd v1.5.7).
#[inline]
fn abs64(s64: i64) -> u64 {
    (if s64 < 0 { s64.wrapping_neg() } else { s64 }) as u64
}

/// Port of `fpDistance` (external/zstd/lib/compress/zstd_preSplit.c:94-104 @ zstd v1.5.7).
fn fp_distance(fp1: &Fingerprint, fp2: &Fingerprint, hash_log: u32) -> u64 {
    let mut distance: u64 = 0;
    for n in 0..1usize << hash_log {
        distance = distance.wrapping_add(abs64(
            (i64::from(fp1.events[n]).wrapping_mul(fp2.nb_events as i64))
                .wrapping_sub(i64::from(fp2.events[n]).wrapping_mul(fp1.nb_events as i64)),
        ));
    }
    distance
}

/// Port of `compareFingerprints` (external/zstd/lib/compress/zstd_preSplit.c:109-120
/// @ zstd v1.5.7): whether the new events are too different.
fn compare_fingerprints(
    reference: &Fingerprint,
    newfp: &Fingerprint,
    penalty: i32,
    hash_log: u32,
) -> bool {
    let p50 = (reference.nb_events as u64).wrapping_mul(newfp.nb_events as u64);
    let deviation = fp_distance(reference, newfp, hash_log);
    let threshold = p50.wrapping_mul((THRESHOLD_BASE as i64 + i64::from(penalty)) as u64)
        / THRESHOLD_PENALTY_RATE;
    deviation >= threshold
}

/// Port of `mergeEvents` (external/zstd/lib/compress/zstd_preSplit.c:122-129 @ zstd v1.5.7).
fn merge_events(acc: &mut Fingerprint, newfp: &Fingerprint) {
    for (a, &n) in acc.events.iter_mut().zip(newfp.events.iter()) {
        *a = a.wrapping_add(n);
    }
    acc.nb_events += newfp.nb_events;
}

/// Port of `ZSTD_splitBlock_byChunks` (external/zstd/lib/compress/zstd_preSplit.c:153-185
/// @ zstd v1.5.7): the first 8 KB chunk whose statistics differ from those before it.
fn split_block_by_chunks(block: &[u8], block_size: usize, level: usize) -> usize {
    const SAMPLING_RATES: [usize; 4] = [43, 11, 5, 1];
    const HASH_PARAMS: [u32; 4] = [8, 9, 10, 10];
    let rate = SAMPLING_RATES[level];
    let hash_log = HASH_PARAMS[level];
    let mut past_events = Fingerprint::new();
    let mut new_events = Fingerprint::new();
    let mut penalty = THRESHOLD_PENALTY;

    record_fingerprint(&mut past_events, &block[..CHUNKSIZE], rate, hash_log);
    let mut pos = CHUNKSIZE;
    while pos <= block_size - CHUNKSIZE {
        record_fingerprint(
            &mut new_events,
            &block[pos..pos + CHUNKSIZE],
            rate,
            hash_log,
        );
        if compare_fingerprints(&past_events, &new_events, penalty, hash_log) {
            return pos;
        } else {
            merge_events(&mut past_events, &new_events);
            if penalty > 0 {
                penalty -= 1;
            }
        }
        pos += CHUNKSIZE;
    }
    block_size
}

/// Port of `ZSTD_splitBlock_fromBorders` (external/zstd/lib/compress/zstd_preSplit.c:196-224
/// @ zstd v1.5.7): the block's ends compared, then each with its middle.
fn split_block_from_borders(block: &[u8], block_size: usize) -> usize {
    let mut past_events = Fingerprint::new();
    let mut new_events = Fingerprint::new();
    let mut middle_events = Fingerprint::new();

    hist_add(&mut past_events.events, &block[..SEGMENT_SIZE]);
    hist_add(
        &mut new_events.events,
        &block[block_size - SEGMENT_SIZE..block_size],
    );
    past_events.nb_events = SEGMENT_SIZE;
    new_events.nb_events = SEGMENT_SIZE;
    if !compare_fingerprints(&past_events, &new_events, 0, 8) {
        return block_size;
    }

    let mid = block_size / 2 - SEGMENT_SIZE / 2;
    hist_add(&mut middle_events.events, &block[mid..mid + SEGMENT_SIZE]);
    middle_events.nb_events = SEGMENT_SIZE;
    let dist_from_begin = fp_distance(&past_events, &middle_events, 8);
    let dist_from_end = fp_distance(&new_events, &middle_events, 8);
    let min_distance = (SEGMENT_SIZE * SEGMENT_SIZE / 3) as u64;
    if abs64((dist_from_begin as i64).wrapping_sub(dist_from_end as i64)) < min_distance {
        return 64 * 1024;
    }
    if dist_from_begin > dist_from_end {
        32 * 1024
    } else {
        96 * 1024
    }
}

/// Port of `ZSTD_splitBlock` (external/zstd/lib/compress/zstd_preSplit.c:226-238 @ zstd v1.5.7):
/// the size of the first block of `block[..block_size]`, by the method of `level` (0 to 4).
pub(crate) fn zstd_split_block(block: &[u8], block_size: usize, level: i32) -> usize {
    if level == 0 {
        return split_block_from_borders(block, block_size);
    }
    // level >= 1
    split_block_by_chunks(block, block_size, (level - 1) as usize)
}
