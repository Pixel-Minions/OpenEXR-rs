// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! An exact port of zstd v1.5.7 (`external/zstd` @ v3.5.3 of OpenEXR), the library
//! OpenEXRCore uses for ZSTD: `ZSTD_decompressDCtx`, `ZSTD_getFrameContentSize`,
//! `ZSTD_compressBound` and `ZSTD_compressCCtx`, without dictionaries (Core uses none). The
//! compressor has the greedy, lazy and lazy2 strategies so far (Core's default level among
//! them); the others follow.
//!
//! The portable code is ported. zstd's BMI2 builds, its fast Huffman loops (in C and in
//! x86-64 assembly), its prefetching sequence decoder and its SIMD tag comparison run the
//! same algorithms with other instructions; they are performance work for later (PLAN.md
//! O3). Custom allocators, dictionaries, legacy formats and streaming are n/a.

mod bitstream;
mod clevels;
mod entropy_common;
mod error_private;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod fse_compress;
mod fse_decompress;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod hist;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod huf_compress;
mod huf_decompress;
mod mem;
mod xxhash;
mod zstd_compress;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod zstd_compress_internal;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod zstd_compress_literals;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod zstd_compress_sequences;
mod zstd_decompress;
mod zstd_decompress_block;
mod zstd_decompress_internal;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod zstd_lazy;
#[allow(dead_code)] // the compressor's, used with zstd_compress.c
mod zstd_pre_split;

pub use error_private::{ErrorCode, ZResult};
pub use zstd_compress::{CCtx, compress_bound};
pub use zstd_decompress::{CONTENTSIZE_ERROR, CONTENTSIZE_UNKNOWN, DCtx, get_frame_content_size};
