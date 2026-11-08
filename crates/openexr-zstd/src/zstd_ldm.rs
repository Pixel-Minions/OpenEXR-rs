// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_ldm.c` (external/zstd/lib/compress/zstd_ldm.c:1-745 @ zstd v1.5.7) and
//! of `zstd_ldm.h` (external/zstd/lib/compress/zstd_ldm.h:1-109 @ zstd v1.5.7), with the
//! long distance matcher's types of `zstd_compress_internal.h` (lines 199-214, 323-353): the
//! long distance match finder, which `ZSTD_resolveEnableLdm` turns on for btopt and up with
//! a window of 2^27.
//!
//! What `ZSTD_compressCCtx` reaches is ported: the parameters, the gear hash, the
//! sequences of each block (with the matcher's own overflow correction), and the raw
//! sequence store the optimal parser takes them from. The matcher's window is the frame's
//! alone (initialized at each reset) and never has an extDict (`ZSTD_window_enforceMaxDist`
//! raises `dictLimit` with `lowLimit`, and a correction lowers both alike), so the extDict
//! branches are n/a. n/a too: `ZSTD_ldm_fillHashTable` (dictionaries), and the block
//! compressor's path below btopt (`ZSTD_ldm_skipSequences`, `maybeSplitSequence`,
//! `ZSTD_ldm_limitTableUpdate`, `ZSTD_ldm_fillFastTables`), which only an explicitly
//! enabled matcher reaches.

use crate::error_private::ErrorCode;
use crate::xxhash::xxh64;
use crate::zstd_compress_internal::{
    CParams, HASH_READ_SIZE, Strategy, Window, ZSTD_BTULTRA, zstd_count,
};
use crate::zstd_ldm_geartab::ZSTD_LDM_GEAR_TAB;

/// `LDM_BUCKET_SIZE_LOG` (external/zstd/lib/compress/zstd_ldm.c:19 @ zstd v1.5.7).
const LDM_BUCKET_SIZE_LOG: u32 = 4;
/// `LDM_MIN_MATCH_LENGTH` (external/zstd/lib/compress/zstd_ldm.c:20 @ zstd v1.5.7).
const LDM_MIN_MATCH_LENGTH: u32 = 64;
/// `LDM_BATCH_SIZE` (external/zstd/lib/compress/zstd_compress_internal.h:335 @ zstd v1.5.7).
const LDM_BATCH_SIZE: usize = 64;
/// `ZSTD_LDM_BUCKETSIZELOG_MAX` (external/zstd/lib/zstd.h:1300 @ zstd v1.5.7).
const ZSTD_LDM_BUCKETSIZELOG_MAX: u32 = 8;
/// `ZSTD_HASHLOG_MIN` (external/zstd/lib/zstd.h:1268 @ zstd v1.5.7).
const ZSTD_HASHLOG_MIN: u32 = 6;
/// `ZSTD_HASHLOG_MAX` on 64-bit (external/zstd/lib/zstd.h:1267 @ zstd v1.5.7).
const ZSTD_HASHLOG_MAX: u32 = 30;

/// Port of `rawSeq` (external/zstd/lib/compress/zstd_compress_internal.h:199-203
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RawSeq {
    /// Offset of sequence.
    pub(crate) offset: u32,
    /// Length of literals prior to match.
    pub(crate) lit_length: u32,
    /// Raw length of match.
    pub(crate) match_length: u32,
}

/// Port of `RawSeqStore_t` (external/zstd/lib/compress/zstd_compress_internal.h:205-212
/// @ zstd v1.5.7): `seq` holds the `size` sequences, at most `capacity`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RawSeqStore {
    pub(crate) seq: Vec<RawSeq>,
    /// The index in seq where reading stopped. pos <= size.
    pub(crate) pos: usize,
    /// The position within the sequence at seq[pos] where reading stopped.
    pub(crate) pos_in_sequence: usize,
    pub(crate) capacity: usize,
}

impl RawSeqStore {
    /// The number of sequences.
    #[inline]
    pub(crate) fn size(&self) -> usize {
        self.seq.len()
    }
}

/// Port of `ldmEntry_t` (external/zstd/lib/compress/zstd_compress_internal.h:323-326
/// @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LdmEntry {
    pub(crate) offset: u32,
    pub(crate) checksum: u32,
}

/// Port of `ldmParams_t` (external/zstd/lib/compress/zstd_compress_internal.h:346-353
/// @ zstd v1.5.7); `enable` is the resolved `enableLdm`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct LdmParams {
    pub(crate) enable: bool,
    /// Log size of hashTable.
    pub(crate) hash_log: u32,
    /// Log bucket size for collision resolution, at most 8.
    pub(crate) bucket_size_log: u32,
    /// Minimum match length.
    pub(crate) min_match_length: u32,
    /// Log number of entries to skip.
    pub(crate) hash_rate_log: u32,
    /// Window log for the LDM.
    pub(crate) window_log: u32,
}

/// Port of `ldmState_t` (external/zstd/lib/compress/zstd_compress_internal.h:337-344
/// @ zstd v1.5.7), without a dictionary (`loadedDictEnd` is 0); the split and candidate
/// arrays are each batch's own.
#[derive(Debug, Clone)]
pub(crate) struct LdmState {
    /// State for the window round buffer management.
    pub(crate) window: Window,
    pub(crate) hash_table: Vec<LdmEntry>,
    /// Next position in bucket to insert entry.
    pub(crate) bucket_offsets: Vec<u8>,
}

impl LdmState {
    pub(crate) fn new() -> Self {
        LdmState {
            window: Window::init(),
            hash_table: Vec::new(),
            bucket_offsets: Vec::new(),
        }
    }
}

/// Port of `ZSTD_resolveEnableLdm` (external/zstd/lib/compress/zstd_compress.c:269-273
/// @ zstd v1.5.7), from `ZSTD_ps_auto`.
pub(crate) fn resolve_enable_ldm(cparams: &CParams) -> bool {
    use crate::zstd_compress_internal::ZSTD_BTOPT;
    cparams.strategy >= ZSTD_BTOPT && cparams.window_log >= 27
}

/// `BOUNDED` (external/zstd/lib/common/zstd_internal.h:58 @ zstd v1.5.7).
fn bounded(min: u32, val: u32, max: u32) -> u32 {
    if val < min {
        min
    } else if val > max {
        max
    } else {
        val
    }
}

/// Port of `ZSTD_ldm_adjustParameters` (external/zstd/lib/compress/zstd_ldm.c:135-167
/// @ zstd v1.5.7), from the parameters a context starts with (all 0).
pub(crate) fn adjust_parameters(params: &mut LdmParams, cparams: &CParams) {
    params.window_log = cparams.window_log;
    let strategy: Strategy = cparams.strategy;
    if params.hash_rate_log == 0 {
        if params.hash_log > 0 {
            // if params->hashLog is set, derive hashRateLog from it
            if params.window_log > params.hash_log {
                params.hash_rate_log = params.window_log - params.hash_log;
            }
        } else {
            // mapping from [fast, rate7] to [btultra2, rate4]
            params.hash_rate_log = 7 - (strategy / 3);
        }
    }
    if params.hash_log == 0 {
        params.hash_log = bounded(
            ZSTD_HASHLOG_MIN,
            params.window_log.wrapping_sub(params.hash_rate_log),
            ZSTD_HASHLOG_MAX,
        );
    }
    if params.min_match_length == 0 {
        params.min_match_length = LDM_MIN_MATCH_LENGTH;
        if strategy >= ZSTD_BTULTRA {
            params.min_match_length /= 2;
        }
    }
    if params.bucket_size_log == 0 {
        params.bucket_size_log = bounded(LDM_BUCKET_SIZE_LOG, strategy, ZSTD_LDM_BUCKETSIZELOG_MAX);
    }
    params.bucket_size_log = params.bucket_size_log.min(params.hash_log);
}

/// Port of `ZSTD_ldm_getMaxNbSeq` (external/zstd/lib/compress/zstd_ldm.c:179-182 @ zstd v1.5.7).
pub(crate) fn get_max_nb_seq(params: &LdmParams, max_chunk_size: usize) -> usize {
    if params.enable {
        max_chunk_size / params.min_match_length as usize
    } else {
        0
    }
}

/// Port of `ldmRollingHashState_t` (external/zstd/lib/compress/zstd_ldm.c:23-26 @ zstd v1.5.7).
struct RollingHashState {
    rolling: u64,
    stop_mask: u64,
}

/// Port of `ZSTD_ldm_gear_init` (external/zstd/lib/compress/zstd_ldm.c:32-58 @ zstd v1.5.7):
/// the rolling hash state honoring the settings in `params`.
fn gear_init(params: &LdmParams) -> RollingHashState {
    let max_bits_in_mask = params.min_match_length.min(64);
    let hash_rate_log = params.hash_rate_log;
    // (the mask's bits are the hash's highest the match length allows, a split triggering
    // on average every 2^hashRateLog bytes)
    let stop_mask = if hash_rate_log > 0 && hash_rate_log <= max_bits_in_mask {
        ((1u64 << hash_rate_log) - 1) << (max_bits_in_mask - hash_rate_log)
    } else {
        // In this degenerate case we simply honor the hash rate.
        (1u64 << hash_rate_log) - 1
    };
    RollingHashState {
        rolling: u64::from(!0u32),
        stop_mask,
    }
}

/// Port of `ZSTD_ldm_gear_reset` (external/zstd/lib/compress/zstd_ldm.c:65-85 @ zstd v1.5.7):
/// `data[..min_match_length]` fed into the hash without registering any splits. Upstream
/// keeps the hash in a local it never stores back, so the state is unchanged.
fn gear_reset(state: &RollingHashState, data: &[u8], min_match_length: usize) {
    let mut hash = state.rolling;
    for &b in &data[..min_match_length] {
        hash = (hash << 1).wrapping_add(ZSTD_LDM_GEAR_TAB[usize::from(b)]);
    }
    let _ = hash;
}

/// Port of `ZSTD_ldm_gear_feed` (external/zstd/lib/compress/zstd_ldm.c:96-133 @ zstd v1.5.7):
/// the split points of `data` (each the count of bytes up to and with it) into `splits`,
/// stopping when `LDM_BATCH_SIZE` are found; returns the bytes processed.
fn gear_feed(state: &mut RollingHashState, data: &[u8], splits: &mut Vec<usize>) -> usize {
    let mut hash = state.rolling;
    let mask = state.stop_mask;
    let mut n = 0;
    while n < data.len() {
        hash = (hash << 1).wrapping_add(ZSTD_LDM_GEAR_TAB[usize::from(data[n])]);
        n += 1;
        if hash & mask == 0 {
            splits.push(n);
            if splits.len() == LDM_BATCH_SIZE {
                break;
            }
        }
    }
    state.rolling = hash;
    n
}

/// Port of `ZSTD_ldm_insertEntry` (external/zstd/lib/compress/zstd_ldm.c:194-204 @ zstd v1.5.7),
/// with `ZSTD_ldm_getBucket` (lines 186-190): `entry` into the bucket of `hash`, at its
/// next position.
fn insert_entry(state: &mut LdmState, hash: usize, entry: LdmEntry, bucket_size_log: u32) {
    let offset = state.bucket_offsets[hash];
    state.hash_table[(hash << bucket_size_log) + usize::from(offset)] = entry;
    state.bucket_offsets[hash] = ((u32::from(offset) + 1) & ((1u32 << bucket_size_log) - 1)) as u8;
}

/// Port of `ZSTD_ldm_countBackwardsMatch` (external/zstd/lib/compress/zstd_ldm.c:210-221
/// @ zstd v1.5.7): the bytes that match backwards before `p_in` and `p_match`, counting only
/// those where `p_match >= p_match_base` and `p_in >= p_anchor`.
fn count_backwards_match(
    src: &[u8],
    mut p_in: usize,
    p_anchor: usize,
    mut p_match: usize,
    p_match_base: usize,
) -> usize {
    let mut match_length = 0;
    while p_in > p_anchor && p_match > p_match_base && src[p_in - 1] == src[p_match - 1] {
        p_in -= 1;
        p_match -= 1;
        match_length += 1;
    }
    match_length
}

/// A match candidate of a batch (`ldmMatchCandidate_t`,
/// external/zstd/lib/compress/zstd_compress_internal.h:328-333 @ zstd v1.5.7), its bucket by
/// index.
struct MatchCandidate {
    split: usize,
    hash: usize,
    checksum: u32,
}

/// Port of `ZSTD_ldm_generateSequences_internal` (external/zstd/lib/compress/zstd_ldm.c:342-512
/// @ zstd v1.5.7), without an extDict: the sequences of `src[istart..iend]` into
/// `raw_seq_store`; returns the literals left after the last one.
fn generate_sequences_internal(
    state: &mut LdmState,
    raw_seq_store: &mut RawSeqStore,
    params: &LdmParams,
    src: &[u8],
    istart: usize,
    iend: usize,
) -> Result<usize, ErrorCode> {
    // LDM parameters
    let min_match_length = params.min_match_length as usize;
    let ents_per_bucket = 1usize << params.bucket_size_log;
    let h_bits = params.hash_log - params.bucket_size_log;
    // Prefix parameters
    let dict_limit = state.window.dict_limit;
    let lowest_index = dict_limit;
    let low_prefix_ptr = state.window.pos(dict_limit);
    // Input bounds
    let ilimit = iend - HASH_READ_SIZE as usize;
    // Input positions
    let mut anchor = istart;
    let mut ip = istart;

    if iend - istart < min_match_length {
        return Ok(iend - anchor);
    }

    // Initialize the rolling hash state with the first minMatchLength bytes
    let mut hash_state = gear_init(params);
    gear_reset(&hash_state, &src[ip..], min_match_length);
    ip += min_match_length;

    let mut splits: Vec<usize> = Vec::with_capacity(LDM_BATCH_SIZE);
    let mut candidates: Vec<MatchCandidate> = Vec::with_capacity(LDM_BATCH_SIZE);
    while ip < ilimit {
        splits.clear();
        let hashed = gear_feed(&mut hash_state, &src[ip..ilimit], &mut splits);

        candidates.clear();
        for &s in &splits {
            let split = ip + s - min_match_length;
            let xxhash = xxh64(&src[split..split + min_match_length], 0);
            let hash = (xxhash as u32 & ((1u32 << h_bits) - 1)) as usize;
            candidates.push(MatchCandidate {
                split,
                hash,
                checksum: (xxhash >> 32) as u32,
            });
        }

        for c in &candidates {
            let split = c.split;
            let checksum = c.checksum;
            let hash = c.hash;
            let bucket = hash << params.bucket_size_log;
            let new_entry = LdmEntry {
                offset: state.window.idx(split),
                checksum,
            };

            // If a split point would generate a sequence overlapping with the previous one,
            // we merely register it in the hash table and move on
            if split < anchor {
                insert_entry(state, hash, new_entry, params.bucket_size_log);
                continue;
            }

            let mut best: Option<(usize, usize)> = None; // (forward, backward)
            let mut best_entry: Option<LdmEntry> = None;
            let mut best_match_length = 0usize;
            for k in 0..ents_per_bucket {
                let cur = state.hash_table[bucket + k];
                if cur.checksum != checksum || cur.offset <= lowest_index {
                    continue;
                }
                let p_match = state.window.pos(cur.offset);
                let cur_forward_match_length = zstd_count(src, split, p_match, iend);
                if cur_forward_match_length < min_match_length {
                    continue;
                }
                let cur_backward_match_length =
                    count_backwards_match(src, split, anchor, p_match, low_prefix_ptr);
                let cur_total_match_length = cur_forward_match_length + cur_backward_match_length;

                if cur_total_match_length > best_match_length {
                    best_match_length = cur_total_match_length;
                    best = Some((cur_forward_match_length, cur_backward_match_length));
                    best_entry = Some(cur);
                }
            }

            // No match found -- insert an entry into the hash table and process the next
            // candidate match
            let (Some((forward_match_length, backward_match_length)), Some(best_entry)) =
                (best, best_entry)
            else {
                insert_entry(state, hash, new_entry, params.bucket_size_log);
                continue;
            };

            // Match found
            let offset = state.window.idx(split).wrapping_sub(best_entry.offset);
            let m_length = forward_match_length + backward_match_length;
            {
                // Out of sequence storage
                if raw_seq_store.size() == raw_seq_store.capacity {
                    return Err(ErrorCode::DstSizeTooSmall);
                }
                raw_seq_store.seq.push(RawSeq {
                    lit_length: (split - backward_match_length - anchor) as u32,
                    match_length: m_length as u32,
                    offset,
                });
            }

            // Insert the current entry into the hash table --- it must be done after the
            // previous block to avoid clobbering bestEntry
            insert_entry(state, hash, new_entry, params.bucket_size_log);

            anchor = split + forward_match_length;

            // If we find a match that ends after the data that we've hashed then we have a
            // repeating, overlapping, pattern. E.g. all zeros. If one repetition of the
            // pattern matches our `stopMask` then all repetitions will. We don't need to
            // insert them all into out table, only the first one. So skip over overlapping
            // matches. This is a major speed boost (20x) for compressing a single byte
            // repeated, when that byte ends up in the table.
            if anchor > ip + hashed {
                gear_reset(
                    &hash_state,
                    &src[anchor - min_match_length..],
                    min_match_length,
                );
                // Continue the outer loop at anchor (ip + hashed == anchor).
                ip = anchor - hashed;
                break;
            }
        }

        ip += hashed;
    }

    Ok(iend - anchor)
}

/// Port of `ZSTD_ldm_reduceTable` (external/zstd/lib/compress/zstd_ldm.c:516-524 @ zstd v1.5.7):
/// the table's indexes lowered by `reducer_value`, or set to 0 below it.
fn reduce_table(table: &mut [LdmEntry], reducer_value: u32) {
    for e in table {
        if e.offset < reducer_value {
            e.offset = 0;
        } else {
            e.offset -= reducer_value;
        }
    }
}

/// Port of `ZSTD_ldm_generateSequences` (external/zstd/lib/compress/zstd_ldm.c:526-601
/// @ zstd v1.5.7): the sequences of `src[istart..iend]` into `sequences`, in chunks of 1 MiB,
/// each after the matcher's overflow correction and maximum distance; the literals left
/// after a chunk's last sequence are added to the next one's.
pub(crate) fn generate_sequences(
    state: &mut LdmState,
    sequences: &mut RawSeqStore,
    params: &LdmParams,
    src: &[u8],
    istart: usize,
    iend: usize,
) -> Result<(), ErrorCode> {
    let max_dist = 1u32 << params.window_log;
    let src_size = iend - istart;
    const K_MAX_CHUNK_SIZE: usize = 1 << 20;
    let nb_chunks = (src_size / K_MAX_CHUNK_SIZE) + usize::from(src_size % K_MAX_CHUNK_SIZE != 0);
    let mut leftover_size = 0usize;

    // The input could be very large (in zstdmt), so it must be broken up into chunks to
    // enforce the maximum distance and handle overflow correction.
    let mut chunk = 0;
    while chunk < nb_chunks && sequences.size() < sequences.capacity {
        let chunk_start = istart + chunk * K_MAX_CHUNK_SIZE;
        let remaining = iend - chunk_start;
        let chunk_end = if remaining < K_MAX_CHUNK_SIZE {
            iend
        } else {
            chunk_start + K_MAX_CHUNK_SIZE
        };
        let chunk_size = chunk_end - chunk_start;
        let prev_size = sequences.size();

        // 1. Perform overflow correction if necessary.
        if state
            .window
            .need_overflow_correction(0, max_dist, chunk_start, chunk_end)
        {
            let correction = state.window.correct_overflow(0, max_dist, chunk_start);
            reduce_table(&mut state.hash_table, correction);
        }
        // 2. We enforce the maximum offset allowed.
        state.window.enforce_max_dist(chunk_end, max_dist);
        // 3. Generate the sequences for the chunk, and get newLeftoverSize.
        let new_leftover_size =
            generate_sequences_internal(state, sequences, params, src, chunk_start, chunk_end)?;
        // 4. We add the leftover literals from previous iterations to the first newly
        //    generated sequence, or add the `newLeftoverSize` if none are generated.
        // Prepend the leftover literals from the last call
        if prev_size < sequences.size() {
            let s = &mut sequences.seq[prev_size];
            s.lit_length = s.lit_length.wrapping_add(leftover_size as u32);
            leftover_size = new_leftover_size;
        } else {
            leftover_size += chunk_size;
        }
        chunk += 1;
    }
    Ok(())
}

/// Port of `ZSTD_ldm_skipRawSeqStoreBytes` (external/zstd/lib/compress/zstd_ldm.c:664-679
/// @ zstd v1.5.7): `nb_bytes` further into the store, `pos` and `posInSequence` moved on.
pub(crate) fn skip_raw_seq_store_bytes(raw_seq_store: &mut RawSeqStore, nb_bytes: usize) {
    let mut curr_pos = (raw_seq_store.pos_in_sequence + nb_bytes) as u32;
    while curr_pos != 0 && raw_seq_store.pos < raw_seq_store.size() {
        let curr_seq = raw_seq_store.seq[raw_seq_store.pos];
        if curr_pos >= curr_seq.lit_length + curr_seq.match_length {
            curr_pos -= curr_seq.lit_length + curr_seq.match_length;
            raw_seq_store.pos += 1;
        } else {
            raw_seq_store.pos_in_sequence = curr_pos as usize;
            break;
        }
    }
    if curr_pos == 0 || raw_seq_store.pos == raw_seq_store.size() {
        raw_seq_store.pos_in_sequence = 0;
    }
}

#[cfg(test)]
mod tests {
    /// The gear table is upstream's, value for value (read from the header itself).
    #[test]
    fn the_gear_table_is_upstreams() {
        let h = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../upstream/openexr/external/zstd/lib/compress/zstd_ldm_geartab.h"),
        )
        .unwrap();
        let start = h.find("ZSTD_ldm_gearTab[256] = {").unwrap();
        let end = start + h[start..].find("};").unwrap();
        let vals: Vec<u64> = h[start..end]
            .split(|c: char| !c.is_ascii_hexdigit() && c != 'x')
            .filter(|t| t.starts_with("0x"))
            .map(|t| u64::from_str_radix(&t[2..], 16).unwrap())
            .collect();
        assert_eq!(vals, super::ZSTD_LDM_GEAR_TAB.to_vec());
    }
}
