// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `zlib_compress.c` (external/deflate/lib/zlib_compress.c:1-82 @ libdeflate v1.26), so far
//! its output bound; `libdeflate_zlib_compress` follows with the compressor.

use crate::deflate_compress::deflate_compress_bound;
use crate::zlib_constants::ZLIB_MIN_OVERHEAD;

/// Port of `libdeflate_zlib_compress_bound` (external/deflate/lib/zlib_compress.c:76-82
/// @ libdeflate v1.26): the most bytes a zlib stream of `in_nbytes` bytes takes.
pub fn zlib_compress_bound(in_nbytes: usize) -> usize {
    ZLIB_MIN_OVERHEAD.wrapping_add(deflate_compress_bound(in_nbytes))
}
