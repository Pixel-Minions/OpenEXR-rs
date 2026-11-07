// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `deflate_compress.c` (external/deflate/lib/deflate_compress.c:1-4135 @ libdeflate v1.26),
//! so far its output bound; the compressor itself follows with ZIP's compression side.

/// Port of `MIN_BLOCK_LENGTH` (external/deflate/lib/deflate_compress.c:66 @ libdeflate v1.26):
/// the minimum block length, in uncompressed bytes, except for the final block if needed.
const MIN_BLOCK_LENGTH: usize = 5000;

/// Port of `libdeflate_deflate_compress_bound` (external/deflate/lib/deflate_compress.c:4087-4135
/// @ libdeflate v1.26): the most bytes a DEFLATE stream of `in_nbytes` bytes takes, at any
/// compression level (upstream's compressor argument is unused).
pub fn deflate_compress_bound(in_nbytes: usize) -> usize {
    // Since the compressor never uses a compressed block when an uncompressed block is
    // cheaper, the worst case can be no worse than the case where only uncompressed blocks
    // are used. Each uncompressed block has 5 bytes of overhead, for the BFINAL, BTYPE, LEN,
    // and NLEN fields; empty inputs need 1 (empty) block.
    let max_blocks = in_nbytes.div_ceil(MIN_BLOCK_LENGTH).max(1);
    5usize.wrapping_mul(max_blocks).wrapping_add(in_nbytes)
}
