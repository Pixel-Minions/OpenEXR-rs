// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! An exact port of zstd v1.5.7 (`external/zstd` @ v3.5.3 of OpenEXR), the library
//! OpenEXRCore uses for ZSTD, so far its decompressor: `ZSTD_decompressDCtx` and
//! `ZSTD_getFrameContentSize`, without dictionaries (Core uses none). The compressor
//! follows with ZSTD's compression side.
//!
//! The portable code is ported. zstd's BMI2 builds, its fast Huffman loops (in C and in
//! x86-64 assembly) and its prefetching sequence decoder decode the same data with other
//! instructions; they are performance work for later (PLAN.md O3). Custom allocators,
//! dictionaries, legacy formats and streaming are n/a.

mod bitstream;
mod entropy_common;
mod error_private;
mod fse_decompress;
mod huf_decompress;
mod mem;
mod xxhash;
mod zstd_decompress;
mod zstd_decompress_block;
mod zstd_decompress_internal;

pub use error_private::{ErrorCode, ZResult};
pub use zstd_decompress::{CONTENTSIZE_ERROR, CONTENTSIZE_UNKNOWN, DCtx, get_frame_content_size};
