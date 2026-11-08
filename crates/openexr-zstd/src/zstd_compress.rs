// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! Port of `zstd_compress.c` (external/zstd/lib/compress/zstd_compress.c:1-7843 @ zstd v1.5.7):
//! `ZSTD_compressCCtx`, a one-shot frame at a compression level, which is how OpenEXRCore
//! compresses (a context per thread, no dictionary, no checksum).
//!
//! A frame compresses the same whatever the context compressed before. Upstream continues its
//! indices from the last frame and keeps its tables, but the window is cleared, so everything
//! left from earlier frames lies below the window's low limit, where the match finders stop as
//! they do at an empty entry; the port starts each frame on a fresh window and fresh tables.
//! The row match finder's hash salt, which upstream advances at every reset with the hashes
//! the last frames searched, is kept as upstream keeps it, though it changes no output: it is
//! XORed into the whole hash, which relabels the rows and the tags without changing which
//! positions share them; it only tells upstream's leftover tags from new ones.
//!
//! The block compressors ported are the fast, dfast, greedy, lazy and lazy2 ones (levels 1 to
//! 12, by input size, Core's default 5 at every size); a level whose parameters choose a binary
//! tree strategy returns `ParameterUnsupported` until its compressor is ported
//! (docs/deferred.md). Streaming,
//! dictionaries, long-distance matching, the post-block splitter, external sequence producers
//! and the multithreaded mode are n/a: `ZSTD_compressCCtx` never uses them at these levels.

use crate::clevels::{ZSTD_DEFAULT_CPARAMETERS, ZSTD_MAX_CLEVEL};
use crate::error_private::{ErrorCode, ZResult};
use crate::hist::hist_count_fast;
use crate::huf_compress::HufCompressTables;
use crate::mem::highbit32;
use crate::zstd_compress_internal::{
    CParams, DEFAULT_MAX_OFF, EntropyCTables, FseCTables, FseRepeat, LL_DEFAULT_NORM,
    LL_DEFAULT_NORM_LOG, LongLengthType, ML_DEFAULT_NORM, ML_DEFAULT_NORM_LOG, MatchState,
    OF_DEFAULT_NORM, OF_DEFAULT_NORM_LOG, SET_COMPRESSED, SeqStore, Strategy, Window, ZSTD_BTLAZY2,
    ZSTD_BTOPT, ZSTD_DFAST, ZSTD_FAST, ZSTD_GREEDY, ZSTD_LAZY2, ZSTD_ROW_HASH_CACHE_SIZE, ll_code,
    min_gain, ml_code,
};
use crate::zstd_compress_literals::zstd_compress_literals;
use crate::zstd_compress_sequences::{
    zstd_build_ctable, zstd_encode_sequences, zstd_select_encoding_type,
};
use crate::zstd_decompress_internal::{
    BLOCK_HEADER_SIZE, LL_FSE_LOG, LONGNBSEQ, MAX_LL, MAX_ML, MAX_OFF, MAX_SEQ, MIN_CBLOCK_SIZE,
    ML_FSE_LOG, OFF_FSE_LOG, REP_START_VALUE,
};
use crate::zstd_double_fast::compress_block_double_fast;
use crate::zstd_fast::compress_block_fast;
use crate::zstd_lazy::{ZSTD_ROW_HASH_TAG_BITS, bounded_4_6, compress_block_lazy};
use crate::zstd_pre_split::zstd_split_block;

/// `ZSTD_MAGICNUMBER` (external/zstd/lib/zstd.h:142 @ zstd v1.5.7).
const ZSTD_MAGICNUMBER: u32 = 0xFD2F_B528;
/// `ZSTD_CLEVEL_DEFAULT` (external/zstd/lib/zstd.h:134 @ zstd v1.5.7).
const ZSTD_CLEVEL_DEFAULT: i32 = 3;
/// `ZSTD_BLOCKSIZE_MAX` (external/zstd/lib/zstd.h:148 @ zstd v1.5.7).
const ZSTD_BLOCKSIZE_MAX: usize = 1 << 17;
/// `ZSTD_WINDOWLOG_MAX` on 64-bit (external/zstd/lib/zstd.h:1264 @ zstd v1.5.7).
const ZSTD_WINDOWLOG_MAX: u32 = 31;
/// `ZSTD_HASHLOG_MIN` (external/zstd/lib/zstd.h:1268 @ zstd v1.5.7).
const ZSTD_HASHLOG_MIN: u32 = 6;
/// `ZSTD_WINDOWLOG_ABSOLUTEMIN` (external/zstd/lib/common/zstd_internal.h:78 @ zstd v1.5.7).
const ZSTD_WINDOWLOG_ABSOLUTEMIN: u32 = 10;
/// `ZSTD_FRAMEHEADERSIZE_MAX` (external/zstd/lib/zstd.h:1259 @ zstd v1.5.7).
const ZSTD_FRAMEHEADERSIZE_MAX: usize = 18;
/// `ZSTD_TARGETLENGTH_MAX` (external/zstd/lib/zstd.h:1277 @ zstd v1.5.7), `ZSTD_minCLevel`'s
/// negation.
const ZSTD_TARGETLENGTH_MAX: i32 = ZSTD_BLOCKSIZE_MAX as i32;
/// `ZSTD_MAX_INPUT_SIZE` on 64-bit (external/zstd/lib/zstd.h:248 @ zstd v1.5.7).
const ZSTD_MAX_INPUT_SIZE: u64 = 0xFF00FF00FF00FF00;
/// `ZSTD_CURRENT_MAX` on 64-bit (external/zstd/lib/compress/zstd_compress_internal.h:1031
/// @ zstd v1.5.7): above it upstream corrects its indices.
const ZSTD_CURRENT_MAX: u64 = 3500 << 20;
/// `ENTROPY_WORKSPACE_SIZE - (MaxSeq + 1) * sizeof (unsigned)`
/// (external/zstd/lib/compress/zstd_compress_internal.h:442-443 @ zstd v1.5.7), the entropy
/// workspace after the sequences' counts.
const ENTROPY_WKSP_SIZE: usize = ((8 << 10) + 512) + 4 * (MAX_SEQ + 2) - (MAX_SEQ + 1) * 4;
/// `SUSPECT_UNCOMPRESSIBLE_LITERAL_RATIO` (external/zstd/lib/compress/zstd_compress.c:2886
/// @ zstd v1.5.7).
const SUSPECT_UNCOMPRESSIBLE_LITERAL_RATIO: usize = 20;

/// `blockType_e` (external/zstd/lib/common/zstd_internal.h:86 @ zstd v1.5.7).
const BT_RAW: u32 = 0;
const BT_RLE: u32 = 1;
const BT_COMPRESSED: u32 = 2;

/// Port of `ZSTD_compressBound` (external/zstd/lib/compress/zstd_compress.c:70-74
/// @ zstd v1.5.7), with `ZSTD_COMPRESSBOUND` (external/zstd/lib/zstd.h:249 @ zstd v1.5.7): the
/// largest frame of `src_size` bytes.
pub fn compress_bound(src_size: usize) -> ZResult<usize> {
    let r = if src_size as u64 >= ZSTD_MAX_INPUT_SIZE {
        0
    } else {
        src_size
            + (src_size >> 8)
            + if src_size < (128 << 10) {
                ((128 << 10) - src_size) >> 11
            } else {
                0
            }
    };
    if r == 0 {
        return Err(ErrorCode::SrcSizeWrong);
    }
    Ok(r)
}

/// Port of `ZSTD_rowMatchFinderSupported` (external/zstd/lib/compress/zstd_compress.c:225-227
/// @ zstd v1.5.7).
fn row_match_finder_supported(strategy: Strategy) -> bool {
    (ZSTD_GREEDY..=ZSTD_LAZY2).contains(&strategy)
}

/// Port of `ZSTD_resolveRowMatchFinderMode` (external/zstd/lib/compress/zstd_compress.c:238-245
/// @ zstd v1.5.7), from `ZSTD_ps_auto`: whether the row match finder is used.
fn resolve_row_match_finder_mode(cparams: &CParams) -> bool {
    row_match_finder_supported(cparams.strategy) && cparams.window_log > 14
}

/// Port of `ZSTD_cycleLog` (external/zstd/lib/compress/zstd_compress.c:1425-1429 @ zstd v1.5.7).
fn cycle_log(hash_log: u32, strat: Strategy) -> u32 {
    let bt_scale = u32::from(strat >= ZSTD_BTLAZY2);
    hash_log - bt_scale
}

/// Port of `ZSTD_adjustCParams_internal` (external/zstd/lib/compress/zstd_compress.c:1473-1609
/// @ zstd v1.5.7), for `ZSTD_cpm_noAttachDict` without a dictionary and a known size, with
/// `ZSTD_dictAndWindowLog` (lines 1439-1463), which keeps the window log without one: the
/// parameters fitted to the input.
fn adjust_cparams_internal(mut c_par: CParams, src_size: u64) -> CParams {
    let max_window_resize: u64 = 1 << (ZSTD_WINDOWLOG_MAX - 1);

    if src_size <= max_window_resize {
        let t_size = src_size as u32;
        let hash_size_min = 1u32 << ZSTD_HASHLOG_MIN;
        let src_log = if t_size < hash_size_min {
            ZSTD_HASHLOG_MIN
        } else {
            highbit32(t_size - 1) + 1
        };
        if c_par.window_log > src_log {
            c_par.window_log = src_log;
        }
    }
    {
        let dict_and_window_log = c_par.window_log;
        let cycle_log = cycle_log(c_par.chain_log, c_par.strategy);
        if c_par.hash_log > dict_and_window_log + 1 {
            c_par.hash_log = dict_and_window_log + 1;
        }
        if cycle_log > dict_and_window_log {
            c_par.chain_log -= cycle_log - dict_and_window_log;
        }
    }

    if c_par.window_log < ZSTD_WINDOWLOG_ABSOLUTEMIN {
        c_par.window_log = ZSTD_WINDOWLOG_ABSOLUTEMIN; // minimum wlog required for valid frame header
    }

    // We can't hash more than 32-bits in total. So that means that we require:
    // (hashLog - rowLog + 8) <= 32
    if row_match_finder_supported(c_par.strategy) {
        let row_log = bounded_4_6(c_par.search_log);
        let max_row_hash_log = 32 - ZSTD_ROW_HASH_TAG_BITS;
        let max_hash_log = max_row_hash_log + row_log;
        if c_par.hash_log > max_hash_log {
            c_par.hash_log = max_hash_log;
        }
    }
    c_par
}

/// Port of `ZSTD_getCParams_internal` (external/zstd/lib/compress/zstd_compress.c:7759-7782
/// @ zstd v1.5.7), with `ZSTD_getCParamRowSize` (lines 7734-7752), for a known size and no
/// dictionary: the level's parameters for the input.
fn get_cparams_internal(compression_level: i32, src_size_hint: u64) -> CParams {
    let r_size = src_size_hint;
    let table_id = usize::from(r_size <= 256 << 10)
        + usize::from(r_size <= 128 << 10)
        + usize::from(r_size <= 16 << 10);
    let row = if compression_level == 0 {
        ZSTD_CLEVEL_DEFAULT // 0 == default
    } else if compression_level < 0 {
        0 // entry 0 is baseline for fast mode
    } else if compression_level > ZSTD_MAX_CLEVEL {
        ZSTD_MAX_CLEVEL
    } else {
        compression_level
    };
    let mut cp = ZSTD_DEFAULT_CPARAMETERS[table_id][row as usize];
    // acceleration factor
    if compression_level < 0 {
        let clamped_compression_level = if -ZSTD_TARGETLENGTH_MAX > compression_level {
            -ZSTD_TARGETLENGTH_MAX
        } else {
            compression_level
        };
        cp.target_length = (-clamped_compression_level) as u32;
    }
    // refine parameters based on srcSize & dictSize
    adjust_cparams_internal(cp, src_size_hint)
}

/// Port of `ZSTD_bitmix` (external/zstd/lib/compress/zstd_compress.c:1971-1977 @ zstd v1.5.7),
/// after XXH3's rrmxmx.
fn bitmix(mut val: u64, len: u64) -> u64 {
    val ^= val.rotate_right(49) ^ val.rotate_right(24);
    val = val.wrapping_mul(0x9FB21C651E98DF25);
    val ^= (val >> 35).wrapping_add(len);
    val = val.wrapping_mul(0x9FB21C651E98DF25);
    val ^ (val >> 28)
}

/// Port of `ZSTD_compressedBlockState_t`
/// (external/zstd/lib/compress/zstd_compress_internal.h:249-252 @ zstd v1.5.7).
#[derive(Debug, Clone)]
struct CompressedBlockState {
    entropy: EntropyCTables,
    rep: [u32; 3],
}

/// Port of `ZSTD_buildSeqStore`'s result `ZSTD_BuildSeqStore_e`
/// (external/zstd/lib/compress/zstd_compress.c:3262 @ zstd v1.5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildSeqStore {
    NoCompress,
    Compress,
}

/// Port of `ZSTD_CCtx` (external/zstd/lib/compress/zstd_compress_internal.h:472-546
/// @ zstd v1.5.7), as `ZSTD_compressCCtx` uses it.
#[derive(Debug)]
pub struct CCtx {
    ms: MatchState,
    blocks: [CompressedBlockState; 2],
    /// Which of `blocks` is `prevCBlock`.
    prev: usize,
    seq_store: SeqStore,
    huf_wksp: HufCompressTables,
    cparams: CParams,
    use_row_match_finder: bool,
    block_size_max: usize,
    is_first_block: bool,
    consumed_src_size: u64,
    produced_csize: u64,
    /// `ZSTDcs_ending` reached.
    stage_ending: bool,
}

impl std::fmt::Debug for HufCompressTables {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HufCompressTables")
    }
}

impl Default for CCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl CCtx {
    /// Port of `ZSTD_createCCtx` (external/zstd/lib/compress/zstd_compress.c:97-100
    /// @ zstd v1.5.7): a context of zeroes.
    pub fn new() -> Self {
        let cparams = CParams {
            window_log: 0,
            chain_log: 0,
            hash_log: 0,
            search_log: 0,
            min_match: 0,
            target_length: 0,
            strategy: 0,
        };
        let block = CompressedBlockState {
            entropy: EntropyCTables::new(),
            rep: [0; 3],
        };
        CCtx {
            ms: MatchState {
                window: Window::for_frame(),
                next_to_update: 0,
                row_hash_log: 0,
                tag_table: Vec::new(),
                hash_cache: [0; ZSTD_ROW_HASH_CACHE_SIZE as usize],
                hash_salt: 0,
                hash_salt_entropy: 0,
                hash_table: Vec::new(),
                chain_table: Vec::new(),
                cparams,
                lazy_skipping: false,
            },
            blocks: [block.clone(), block],
            prev: 0,
            seq_store: SeqStore::new(),
            huf_wksp: HufCompressTables::new(),
            cparams,
            use_row_match_finder: false,
            block_size_max: 0,
            is_first_block: true,
            consumed_src_size: 0,
            produced_csize: 0,
            stage_ending: false,
        }
    }

    /// Port of `ZSTD_resetCCtx_internal` (external/zstd/lib/compress/zstd_compress.c:2100-2280
    /// @ zstd v1.5.7) for a one-shot frame of `pledged_src_size` bytes, with
    /// `ZSTD_reset_compressedBlockState` (lines 1918-1927), `ZSTD_reset_matchState` (lines
    /// 1985-2069), `ZSTD_invalidateMatchState` (lines 1933-1941) and `ZSTD_advanceHashSalt`
    /// (lines 1980-1982).
    fn reset(&mut self, cparams: CParams, use_row_match_finder: bool, pledged_src_size: u64) {
        self.is_first_block = true;
        self.cparams = cparams;
        self.use_row_match_finder = use_row_match_finder;
        let window_size = {
            let w = 1u64 << cparams.window_log;
            let m = if w < pledged_src_size {
                w
            } else {
                pledged_src_size
            };
            if m < 1 { 1 } else { m as usize }
        };
        self.block_size_max = if ZSTD_BLOCKSIZE_MAX < window_size {
            ZSTD_BLOCKSIZE_MAX
        } else {
            window_size
        };
        self.consumed_src_size = 0;
        self.produced_csize = 0;
        self.stage_ending = false;

        // ZSTD_reset_compressedBlockState (prevCBlock)
        let prev = &mut self.blocks[self.prev];
        prev.rep = REP_START_VALUE;
        prev.entropy.huf.repeat_mode = crate::huf_compress::HufRepeat::None;
        prev.entropy.fse.offcode_repeat_mode = FseRepeat::None;
        prev.entropy.fse.matchlength_repeat_mode = FseRepeat::None;
        prev.entropy.fse.litlength_repeat_mode = FseRepeat::None;

        // ZSTD_reset_matchState
        let ms = &mut self.ms;
        let row = row_match_finder_supported(cparams.strategy) && use_row_match_finder;
        let chain_size = if cparams.strategy != crate::zstd_compress_internal::ZSTD_FAST && !row {
            1usize << cparams.chain_log
        } else {
            0
        };
        let h_size = 1usize << cparams.hash_log;
        ms.window = Window::for_frame();
        ms.lazy_skipping = false;
        ms.next_to_update = ms.window.dict_limit;
        ms.hash_table.clear();
        ms.hash_table.resize(h_size, 0);
        ms.chain_table.clear();
        ms.chain_table.resize(chain_size, 0);
        if row {
            // Row match finder needs an additional table of hashes ("tags")
            ms.tag_table.clear();
            ms.tag_table.resize(h_size, 0);
            ms.hash_salt = bitmix(ms.hash_salt, 8) ^ bitmix(u64::from(ms.hash_salt_entropy), 4);
            // Switch to 32-entry rows if searchLog is 5 (or more)
            let row_log = bounded_4_6(cparams.search_log);
            ms.row_hash_log = cparams.hash_log - row_log;
        }
        ms.cparams = cparams;
        self.seq_store.reset();
    }

    /// Port of `ZSTD_compressCCtx` (external/zstd/lib/compress/zstd_compress.c:5487-5495
    /// @ zstd v1.5.7), with `ZSTD_compress_usingDict` (lines 5472-5485) without a
    /// dictionary, `ZSTD_getParams_internal` (lines 7798-7807), `ZSTD_CCtxParams_init_internal`
    /// (lines 373-393), `ZSTD_compress_advanced_internal` (lines 5458-5470),
    /// `ZSTD_compressBegin_internal` (lines 5238-5283) and `ZSTD_compressEnd_public` (lines
    /// 5407-5431): `src` as one zstd frame into `dst` at `compression_level`.
    pub fn compress_cctx(
        &mut self,
        dst: &mut [u8],
        src: &[u8],
        compression_level: i32,
    ) -> ZResult<usize> {
        let cparams = get_cparams_internal(compression_level, src.len() as u64);
        if !(ZSTD_FAST..=ZSTD_LAZY2).contains(&cparams.strategy) {
            // the binary tree strategies: not ported yet (docs/deferred.md)
            return Err(ErrorCode::ParameterUnsupported);
        }
        if src.len() as u64 >= ZSTD_CURRENT_MAX {
            // upstream corrects its indices past this; not ported (docs/deferred.md)
            return Err(ErrorCode::SrcSizeWrong);
        }
        let use_row = resolve_row_match_finder_mode(&cparams);
        debug_assert!(cparams.strategy < ZSTD_BTOPT); // no LDM, no post block splitter
        self.reset(cparams, use_row, src.len() as u64);

        // ZSTD_compressContinue_internal (frame, last chunk)
        let fh_size = write_frame_header(dst, &cparams, src.len() as u64)?;
        let mut c_size = 0;
        if !src.is_empty() {
            // ZSTD_window_update: a new segment, from index ZSTD_WINDOW_START_INDEX
            self.ms.window.next_src = self.ms.window.idx(src.len());
            self.ms.next_to_update = self.ms.window.dict_limit;
            c_size = self.compress_frame_chunk(&mut dst[fh_size..], src)?;
            self.consumed_src_size += src.len() as u64;
            self.produced_csize += (c_size + fh_size) as u64;
        }
        let c_size = c_size + fh_size;

        // ZSTD_writeEpilogue
        let end_result = {
            let op = &mut dst[c_size..];
            let mut n = 0;
            if !self.stage_ending {
                // write one last empty block, make it the "last" block
                let c_block_header24: u32 = 1 /* last block */ + (BT_RAW << 1);
                if op.len() < 3 {
                    return Err(ErrorCode::DstSizeTooSmall); // no room for epilogue
                }
                op[..3].copy_from_slice(&c_block_header24.to_le_bytes()[..3]);
                n += BLOCK_HEADER_SIZE;
            }
            n
        };
        Ok(c_size + end_result)
    }

    /// Port of `ZSTD_compress_frameChunk` (external/zstd/lib/compress/zstd_compress.c:4591-4692
    /// @ zstd v1.5.7), the last chunk, with `ZSTD_optimalBlockSize` (lines 4552-4582): the
    /// input in blocks.
    fn compress_frame_chunk(&mut self, dst: &mut [u8], src: &[u8]) -> ZResult<usize> {
        let block_size_max = self.block_size_max;
        let mut remaining = src.len();
        let mut ip = 0;
        let mut op = 0;
        let mut dst_capacity = dst.len();
        let max_dist = 1u32 << self.cparams.window_log;
        let mut savings: i64 = self.consumed_src_size as i64 - self.produced_csize as i64;

        while remaining != 0 {
            let block_size = {
                // split level based on compression strategy, from `fast` to `btultra2`
                const SPLIT_LEVELS: [i32; 10] = [0, 0, 1, 2, 2, 3, 3, 4, 4, 4];
                if remaining < 128 << 10 || block_size_max < 128 << 10 {
                    if remaining < block_size_max {
                        remaining
                    } else {
                        block_size_max
                    }
                } else if savings < 3 {
                    128 << 10
                } else {
                    // ZSTD_c_splitAfterSequences is 0: the strategy's level
                    let split_level = SPLIT_LEVELS[self.cparams.strategy as usize];
                    zstd_split_block(&src[ip..], block_size_max, split_level)
                }
            };
            let last_block = u32::from(block_size == remaining);

            if dst_capacity < BLOCK_HEADER_SIZE + MIN_CBLOCK_SIZE + 1 {
                return Err(ErrorCode::DstSizeTooSmall); // not enough space to store compressed block
            }

            self.ms.window.enforce_max_dist(ip + block_size, max_dist);
            // Ensure hash/chain table insertion resumes no sooner than lowlimit
            if self.ms.next_to_update < self.ms.window.low_limit {
                self.ms.next_to_update = self.ms.window.low_limit;
            }

            let c_size = {
                let c_size = self.compress_block_internal(
                    &mut dst[op + BLOCK_HEADER_SIZE..op + dst_capacity],
                    src,
                    ip,
                    block_size,
                )?;
                if c_size == 0 {
                    // block is not compressible
                    no_compress_block(
                        &mut dst[op..op + dst_capacity],
                        &src[ip..ip + block_size],
                        last_block,
                    )?
                } else {
                    let c_block_header = if c_size == 1 {
                        last_block + (BT_RLE << 1) + ((block_size as u32) << 3)
                    } else {
                        last_block + (BT_COMPRESSED << 1) + ((c_size as u32) << 3)
                    };
                    dst[op..op + 3].copy_from_slice(&c_block_header.to_le_bytes()[..3]);
                    c_size + BLOCK_HEADER_SIZE
                }
            };

            savings += block_size as i64 - c_size as i64;
            ip += block_size;
            remaining -= block_size;
            op += c_size;
            dst_capacity -= c_size;
            self.is_first_block = false;
        }

        if op > 0 {
            self.stage_ending = true;
        }
        Ok(op)
    }

    /// Port of `ZSTD_compressBlock_internal`
    /// (external/zstd/lib/compress/zstd_compress.c:4383-4448 @ zstd v1.5.7), in a frame, with `ZSTD_isRLE` (lines 3572-3592) and
    /// `ZSTD_blockState_confirmRepcodesAndEntropyTables` (lines 3607-3612): the block's
    /// compressed size, 1 for an RLE block, 0 to store it raw.
    fn compress_block_internal(
        &mut self,
        dst: &mut [u8],
        src: &[u8],
        istart: usize,
        src_size: usize,
    ) -> ZResult<usize> {
        // This is an estimated upper bound for the length of an rle block.
        const RLE_MAX_LENGTH: usize = 25;
        let mut c_size;

        if self.build_seq_store(src, istart, src_size) == BuildSeqStore::NoCompress {
            c_size = 0;
        } else {
            // encode sequences and literals
            c_size = self.entropy_compress_seq_store(dst, src_size)?;
            let block = &src[istart..istart + src_size];
            if !self.is_first_block
                && c_size < RLE_MAX_LENGTH
                && block.iter().all(|&b| b == block[0])
            {
                c_size = 1;
                dst[0] = block[0];
            }
        }

        // out:
        if c_size > 1 {
            self.prev ^= 1;
        }
        // (an offcode table is FSE_repeat_valid only with a dictionary)
        Ok(c_size)
    }

    /// Port of `ZSTD_buildSeqStore` (external/zstd/lib/compress/zstd_compress.c:3264-3427
    /// @ zstd v1.5.7), with `ZSTD_storeLastLiterals` (lines 3154-3159): the block's sequences,
    /// from the strategy's block compressor.
    fn build_seq_store(&mut self, src: &[u8], istart: usize, src_size: usize) -> BuildSeqStore {
        if src_size < MIN_CBLOCK_SIZE + BLOCK_HEADER_SIZE + 1 + 1 {
            return BuildSeqStore::NoCompress; // don't even attempt compression below a certain srcSize
        }
        self.seq_store.reset();

        // limited update after a very long match
        {
            let curr = self.ms.window.idx(istart);
            if curr > self.ms.next_to_update + 384 {
                let d = curr - self.ms.next_to_update - 384;
                self.ms.next_to_update = curr - if 192 < d { 192 } else { d };
            }
        }

        // select and store sequences
        let next = self.prev ^ 1;
        let prev_rep = self.blocks[self.prev].rep;
        self.blocks[next].rep = prev_rep;
        let row = self.use_row_match_finder && row_match_finder_supported(self.cparams.strategy);
        // ZSTD_selectBlockCompressor (lines 3065-3152), without a dictionary
        let (ms, seq_store, rep) = (
            &mut self.ms,
            &mut self.seq_store,
            &mut self.blocks[next].rep,
        );
        let last_ll_size = match self.cparams.strategy {
            ZSTD_FAST => compress_block_fast(ms, seq_store, rep, src, istart, istart + src_size),
            ZSTD_DFAST => {
                compress_block_double_fast(ms, seq_store, rep, src, istart, istart + src_size)
            }
            s => compress_block_lazy(
                ms,
                seq_store,
                rep,
                src,
                istart,
                istart + src_size,
                row,
                s - ZSTD_GREEDY,
            ),
        };
        let last_literals = istart + src_size - last_ll_size;
        self.seq_store
            .lit
            .extend_from_slice(&src[last_literals..istart + src_size]);
        BuildSeqStore::Compress
    }

    /// Port of `ZSTD_entropyCompressSeqStore`
    /// (external/zstd/lib/compress/zstd_compress.c:3045-3064 @ zstd v1.5.7), with `ZSTD_entropyCompressSeqStore_wExtLitBuffer` (lines
    /// 3006-3042): the block's literals and sequences, or 0 when that doesn't save enough.
    fn entropy_compress_seq_store(&mut self, dst: &mut [u8], block_size: usize) -> ZResult<usize> {
        let dst_capacity = dst.len();
        let c_size = match self.entropy_compress_seq_store_internal(dst) {
            Ok(0) => return Ok(0),
            // When srcSize <= dstCapacity, there is enough space to write a raw uncompressed
            // block. Since we ran out of space, block must be not compressible, so fall back
            // to raw uncompressed block.
            Err(ErrorCode::DstSizeTooSmall) if block_size <= dst_capacity => return Ok(0),
            r => r?,
        };
        // Check compressibility
        let max_csize = block_size - min_gain(block_size, self.cparams.strategy);
        if c_size >= max_csize {
            return Ok(0); // block not compressed
        }
        Ok(c_size)
    }

    /// Port of `ZSTD_entropyCompressSeqStore_internal`
    /// (external/zstd/lib/compress/zstd_compress.c:2888-3003 @ zstd v1.5.7): the literals section, then the sequences section.
    fn entropy_compress_seq_store_internal(&mut self, dst: &mut [u8]) -> ZResult<usize> {
        let strategy = self.cparams.strategy;
        let dst_capacity = dst.len();
        let nb_seq = self.seq_store.sequences.len();
        let (lo, hi) = self.blocks.split_at_mut(1);
        let (prev_entropy, next_entropy) = if self.prev == 0 {
            (&lo[0].entropy, &mut hi[0].entropy)
        } else {
            (&hi[0].entropy, &mut lo[0].entropy)
        };
        let mut op = 0;

        // Compress literals
        {
            let lit_size = self.seq_store.lit.len();
            // Base suspicion of uncompressibility on ratio of literals to sequences
            let suspect_uncompressible =
                (nb_seq == 0) || (lit_size / nb_seq >= SUSPECT_UNCOMPRESSIBLE_LITERAL_RATIO);
            let c_size = zstd_compress_literals(
                dst,
                &self.seq_store.lit,
                &mut self.huf_wksp,
                &prev_entropy.huf,
                &mut next_entropy.huf,
                strategy,
                false, // ZSTD_literalsCompressionIsDisabled: auto, and not ZSTD_fast
                suspect_uncompressible,
                false, // bmi2 builds encode the same bytes
            )?;
            op += c_size;
        }

        // Sequences Header
        if dst_capacity - op < 3 /*max nbSeq Size*/ + 1
        /*seqHead*/
        {
            return Err(ErrorCode::DstSizeTooSmall); // Can't fit seq hdr in output buf!
        }
        if nb_seq < 128 {
            dst[op] = nb_seq as u8;
            op += 1;
        } else if nb_seq < LONGNBSEQ as usize {
            dst[op] = ((nb_seq >> 8) + 0x80) as u8;
            dst[op + 1] = nb_seq as u8;
            op += 2;
        } else {
            dst[op] = 0xFF;
            dst[op + 1..op + 3]
                .copy_from_slice(&((nb_seq - LONGNBSEQ as usize) as u16).to_le_bytes());
            op += 3;
        }
        if nb_seq == 0 {
            // Copy the old tables over as if we repeated them
            next_entropy.fse.copy_from(&prev_entropy.fse);
            return Ok(op);
        }

        let seq_head = op;
        op += 1;
        // build stats for sequences
        let (ll_type, of_type, ml_type, size, last_count_size, long_offsets) =
            build_sequences_statistics(
                &mut self.seq_store,
                &prev_entropy.fse,
                &mut next_entropy.fse,
                &mut dst[op..],
                strategy,
            )?;
        dst[seq_head] = ((ll_type << 6) + (of_type << 4) + (ml_type << 2)) as u8;
        op += size;

        {
            let ss = &self.seq_store;
            let bitstream_size = zstd_encode_sequences(
                &mut dst[op..],
                &next_entropy.fse.matchlength_ctable,
                &ss.ml_code,
                &next_entropy.fse.offcode_ctable,
                &ss.of_code,
                &next_entropy.fse.litlength_ctable,
                &ss.ll_code,
                &ss.sequences,
                long_offsets,
            )?;
            op += bitstream_size;
            // zstd versions <= 1.3.4 mistakenly report corruption when FSE_readNCount()
            // receives a buffer < 4 bytes. In this exceedingly rare case, we will simply emit
            // an uncompressed block, since it isn't worth optimizing.
            if last_count_size != 0 && (last_count_size + bitstream_size) < 4 {
                return Ok(0);
            }
        }
        Ok(op)
    }
}

/// Port of `ZSTD_seqToCodes` (external/zstd/lib/compress/zstd_compress.c:2693-2719
/// @ zstd v1.5.7): the sequences' codes; whether an offset is too long for 32-bit (never on
/// 64-bit).
fn seq_to_codes(ss: &mut SeqStore) -> bool {
    let nb_seq = ss.sequences.len();
    ss.ll_code.clear();
    ss.of_code.clear();
    ss.ml_code.clear();
    for s in &ss.sequences {
        ss.ll_code.push(ll_code(u32::from(s.lit_length)) as u8);
        ss.of_code.push(highbit32(s.off_base) as u8);
        ss.ml_code.push(ml_code(u32::from(s.ml_base)) as u8);
    }
    debug_assert_eq!(ss.ll_code.len(), nb_seq);
    if ss.long_length_type == LongLengthType::LiteralLength {
        ss.ll_code[ss.long_length_pos as usize] = MAX_LL as u8;
    }
    if ss.long_length_type == LongLengthType::MatchLength {
        ss.ml_code[ss.long_length_pos as usize] = MAX_ML as u8;
    }
    false
}

/// Port of `ZSTD_buildSequencesStatistics` (external/zstd/lib/compress/zstd_compress.c:2763-2880
/// @ zstd v1.5.7): each code's table chosen and described into `dst`; returns the three
/// types, the descriptions' size, the last new table's description size and `longOffsets`.
fn build_sequences_statistics(
    ss: &mut SeqStore,
    prev_entropy: &FseCTables,
    next_entropy: &mut FseCTables,
    dst: &mut [u8],
    strategy: Strategy,
) -> ZResult<(u32, u32, u32, usize, usize, bool)> {
    let nb_seq = ss.sequences.len();
    let mut count = [0u32; MAX_SEQ + 1];
    let mut op = 0;
    let mut last_count_size = 0;

    // convert length/distances into codes
    let long_offsets = seq_to_codes(ss);

    // build CTable for Literal Lengths
    let ll_type = {
        let mut max = MAX_LL;
        let most_frequent = hist_count_fast(&mut count, &mut max, &ss.ll_code)? as usize; // can't fail
        next_entropy.litlength_repeat_mode = prev_entropy.litlength_repeat_mode;
        let t = zstd_select_encoding_type(
            &mut next_entropy.litlength_repeat_mode,
            &count,
            max,
            most_frequent,
            nb_seq,
            LL_FSE_LOG,
            &prev_entropy.litlength_ctable,
            &LL_DEFAULT_NORM,
            LL_DEFAULT_NORM_LOG,
            true,
            strategy,
        );
        let count_size = zstd_build_ctable(
            &mut dst[op..],
            &mut next_entropy.litlength_ctable,
            LL_FSE_LOG,
            t,
            &mut count,
            max,
            &ss.ll_code,
            &LL_DEFAULT_NORM,
            LL_DEFAULT_NORM_LOG,
            MAX_LL,
            &prev_entropy.litlength_ctable,
            ENTROPY_WKSP_SIZE,
        )?;
        if t == SET_COMPRESSED {
            last_count_size = count_size;
        }
        op += count_size;
        t
    };

    // build CTable for Offsets
    let of_type = {
        let mut max = MAX_OFF;
        let most_frequent = hist_count_fast(&mut count, &mut max, &ss.of_code)? as usize; // can't fail
        // We can only use the basic table if max <= DefaultMaxOff, otherwise the offsets are too large
        let default_policy = max <= DEFAULT_MAX_OFF;
        next_entropy.offcode_repeat_mode = prev_entropy.offcode_repeat_mode;
        let t = zstd_select_encoding_type(
            &mut next_entropy.offcode_repeat_mode,
            &count,
            max,
            most_frequent,
            nb_seq,
            OFF_FSE_LOG,
            &prev_entropy.offcode_ctable,
            &OF_DEFAULT_NORM,
            OF_DEFAULT_NORM_LOG,
            default_policy,
            strategy,
        );
        let count_size = zstd_build_ctable(
            &mut dst[op..],
            &mut next_entropy.offcode_ctable,
            OFF_FSE_LOG,
            t,
            &mut count,
            max,
            &ss.of_code,
            &OF_DEFAULT_NORM,
            OF_DEFAULT_NORM_LOG,
            DEFAULT_MAX_OFF,
            &prev_entropy.offcode_ctable,
            ENTROPY_WKSP_SIZE,
        )?;
        if t == SET_COMPRESSED {
            last_count_size = count_size;
        }
        op += count_size;
        t
    };

    // build CTable for MatchLengths
    let ml_type = {
        let mut max = MAX_ML;
        let most_frequent = hist_count_fast(&mut count, &mut max, &ss.ml_code)? as usize; // can't fail
        next_entropy.matchlength_repeat_mode = prev_entropy.matchlength_repeat_mode;
        let t = zstd_select_encoding_type(
            &mut next_entropy.matchlength_repeat_mode,
            &count,
            max,
            most_frequent,
            nb_seq,
            ML_FSE_LOG,
            &prev_entropy.matchlength_ctable,
            &ML_DEFAULT_NORM,
            ML_DEFAULT_NORM_LOG,
            true,
            strategy,
        );
        let count_size = zstd_build_ctable(
            &mut dst[op..],
            &mut next_entropy.matchlength_ctable,
            ML_FSE_LOG,
            t,
            &mut count,
            max,
            &ss.ml_code,
            &ML_DEFAULT_NORM,
            ML_DEFAULT_NORM_LOG,
            MAX_ML,
            &prev_entropy.matchlength_ctable,
            ENTROPY_WKSP_SIZE,
        )?;
        if t == SET_COMPRESSED {
            last_count_size = count_size;
        }
        op += count_size;
        t
    };

    Ok((ll_type, of_type, ml_type, op, last_count_size, long_offsets))
}

/// Port of `ZSTD_writeFrameHeader` (external/zstd/lib/compress/zstd_compress.c:4695-4743
/// @ zstd v1.5.7), with the content size, no dictionary ID and no checksum.
fn write_frame_header(dst: &mut [u8], cparams: &CParams, pledged_src_size: u64) -> ZResult<usize> {
    let dict_id_size_code = 0u32;
    let checksum_flag = 0u32;
    let window_size = 1u32 << cparams.window_log;
    let single_segment = u32::from(u64::from(window_size) >= pledged_src_size);
    let window_log_byte = ((cparams.window_log - ZSTD_WINDOWLOG_ABSOLUTEMIN) << 3) as u8;
    let fcs_code = u32::from(pledged_src_size >= 256)
        + u32::from(pledged_src_size >= 65536 + 256)
        + u32::from(pledged_src_size >= 0xFFFFFFFF);
    let frame_header_description_byte =
        (dict_id_size_code + (checksum_flag << 2) + (single_segment << 5) + (fcs_code << 6)) as u8;
    if dst.len() < ZSTD_FRAMEHEADERSIZE_MAX {
        return Err(ErrorCode::DstSizeTooSmall); // dst buf is too small to fit worst-case frame header size.
    }
    dst[..4].copy_from_slice(&ZSTD_MAGICNUMBER.to_le_bytes());
    let mut pos = 4;
    dst[pos] = frame_header_description_byte;
    pos += 1;
    if single_segment == 0 {
        dst[pos] = window_log_byte;
        pos += 1;
    }
    match fcs_code {
        0 => {
            if single_segment != 0 {
                dst[pos] = pledged_src_size as u8;
                pos += 1;
            }
        }
        1 => {
            dst[pos..pos + 2].copy_from_slice(&((pledged_src_size - 256) as u16).to_le_bytes());
            pos += 2;
        }
        2 => {
            dst[pos..pos + 4].copy_from_slice(&(pledged_src_size as u32).to_le_bytes());
            pos += 4;
        }
        _ => {
            dst[pos..pos + 8].copy_from_slice(&pledged_src_size.to_le_bytes());
            pos += 8;
        }
    }
    Ok(pos)
}

/// Port of `ZSTD_noCompressBlock` (external/zstd/lib/compress/zstd_compress_internal.h:649-659
/// @ zstd v1.5.7): the block stored raw.
fn no_compress_block(dst: &mut [u8], src: &[u8], last_block: u32) -> ZResult<usize> {
    let c_block_header24 = last_block + (BT_RAW << 1) + ((src.len() as u32) << 3);
    if src.len() + BLOCK_HEADER_SIZE > dst.len() {
        return Err(ErrorCode::DstSizeTooSmall); // dst buf too small for uncompressed block
    }
    dst[..3].copy_from_slice(&c_block_header24.to_le_bytes()[..3]);
    dst[BLOCK_HEADER_SIZE..BLOCK_HEADER_SIZE + src.len()].copy_from_slice(src);
    Ok(BLOCK_HEADER_SIZE + src.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zstd_decompress::DCtx;

    fn round_trip(cctx: &mut CCtx, src: &[u8], level: i32) -> Vec<u8> {
        let mut dst = vec![0u8; compress_bound(src.len()).unwrap()];
        let n = cctx.compress_cctx(&mut dst, src, level).unwrap();
        dst.truncate(n);
        let mut out = vec![0u8; src.len()];
        let mut dctx = DCtx::new();
        let m = dctx.decompress_dctx(&mut out, &dst).unwrap();
        assert_eq!(m, src.len());
        assert!(out == src, "level {level}, {} bytes", src.len());
        dst
    }

    /// Image-like rows: smooth ramps with noise, repeated with drift.
    fn image_like(len: usize, seed: u32) -> Vec<u8> {
        let mut x = seed;
        (0..len)
            .map(|i| {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                let noise = (x >> 16) & 7;
                ((i / 3 % 97) as u32 + noise + ((i >> 10) as u32 & 31)) as u8
            })
            .collect()
    }

    #[test]
    fn frames_decode_to_their_input() {
        let mut cctx = CCtx::new();
        for &len in &[
            0usize,
            1,
            6,
            7,
            64,
            1000,
            4096,
            16384,
            16385,
            40000,
            131072,
            131073,
            300000,
            1 << 20,
        ] {
            for level in 1..=ZSTD_MAX_CLEVEL {
                let s = get_cparams_internal(level, len as u64).strategy;
                if (ZSTD_FAST..=ZSTD_LAZY2).contains(&s) {
                    round_trip(&mut cctx, &image_like(len, len as u32), level);
                }
            }
        }
        let zeros = vec![0u8; 300000];
        round_trip(&mut cctx, &zeros, 5);
    }

    #[test]
    fn unported_strategies_are_refused() {
        let mut cctx = CCtx::new();
        let mut dst = vec![0u8; 1024];
        assert_eq!(
            cctx.compress_cctx(&mut dst, &[1, 2, 3], 19),
            Err(ErrorCode::ParameterUnsupported)
        );
    }
}
