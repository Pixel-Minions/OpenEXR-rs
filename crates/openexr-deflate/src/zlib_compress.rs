// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `zlib_compress.c` (external/deflate/lib/zlib_compress.c:1-82 @ libdeflate v1.26):
//! the zlib wrapper around a DEFLATE stream.

use crate::adler32;
use crate::deflate_compress::{Compressor, deflate_compress_bound};
use crate::zlib_constants::{
    ZLIB_CINFO_32K_WINDOW, ZLIB_CM_DEFLATE, ZLIB_DEFAULT_COMPRESSION, ZLIB_FAST_COMPRESSION,
    ZLIB_FASTEST_COMPRESSION, ZLIB_MIN_OVERHEAD, ZLIB_SLOWEST_COMPRESSION,
};

impl Compressor {
    /// Port of `libdeflate_zlib_compress` (external/deflate/lib/zlib_compress.c:34-74
    /// @ libdeflate v1.26): the zlib stream of `input` into `out` (header, DEFLATE stream,
    /// Adler-32); returns its size, or 0 when `out` is too small.
    pub fn zlib_compress(&mut self, input: &[u8], out: &mut [u8]) -> usize {
        if out.len() <= ZLIB_MIN_OVERHEAD {
            return 0;
        }

        // 2 byte header: CMF and FLG
        let mut hdr: u16 = (ZLIB_CM_DEFLATE << 8) | (ZLIB_CINFO_32K_WINDOW << 12);
        let compression_level = self.compression_level();
        let level_hint = if compression_level < 2 {
            ZLIB_FASTEST_COMPRESSION
        } else if compression_level < 6 {
            ZLIB_FAST_COMPRESSION
        } else if compression_level < 8 {
            ZLIB_DEFAULT_COMPRESSION
        } else {
            ZLIB_SLOWEST_COMPRESSION
        };
        hdr |= level_hint << 6;
        hdr |= 31 - (hdr % 31);

        out[..2].copy_from_slice(&hdr.to_be_bytes());
        let mut out_next = 2;

        // Compressed data
        let avail = out.len() - ZLIB_MIN_OVERHEAD;
        let deflate_size = self.deflate_compress(input, &mut out[out_next..out_next + avail]);
        if deflate_size == 0 {
            return 0;
        }
        out_next += deflate_size;

        // ADLER32
        out[out_next..out_next + 4].copy_from_slice(&adler32(1, Some(input)).to_be_bytes());
        out_next += 4;

        out_next
    }
}

/// Port of `libdeflate_zlib_compress_bound` (external/deflate/lib/zlib_compress.c:76-82
/// @ libdeflate v1.26): the most bytes a zlib stream of `in_nbytes` bytes takes.
pub fn zlib_compress_bound(in_nbytes: usize) -> usize {
    ZLIB_MIN_OVERHEAD.wrapping_add(deflate_compress_bound(in_nbytes))
}
