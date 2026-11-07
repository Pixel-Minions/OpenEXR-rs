// SPDX-License-Identifier: BSD-3-Clause
// Copyright (c) Meta Platforms, Inc. and affiliates.

//! An exact port of zstd v1.5.7 (`external/zstd` @ v3.5.3 of OpenEXR), the library
//! OpenEXRCore uses for ZSTD, so far the foundations of its decompressor: the bit stream
//! reader, FSE tables and the Huffman weights reader.
//!
//! The portable code is ported. zstd's BMI2 builds and its x86-64 assembly Huffman decoder
//! compute the same results with other instructions; they are performance work for later
//! (PLAN.md O3). Custom allocators and dictionaries, which Core does not use, are n/a.

// The decompressor is ported module by module; its entry points come with its last one.
#![allow(dead_code)]

mod bitstream;
mod entropy_common;
mod error_private;
mod fse_decompress;
mod mem;

pub use error_private::{ErrorCode, ZResult};
