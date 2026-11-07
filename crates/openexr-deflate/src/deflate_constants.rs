// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `deflate_constants.h` (external/deflate/lib/deflate_constants.h:1-56 @ libdeflate
//! v1.26): constants of the DEFLATE format.

// Valid block types
pub(crate) const DEFLATE_BLOCKTYPE_UNCOMPRESSED: u32 = 0;
pub(crate) const DEFLATE_BLOCKTYPE_STATIC_HUFFMAN: u32 = 1;
pub(crate) const DEFLATE_BLOCKTYPE_DYNAMIC_HUFFMAN: u32 = 2;

/// The minimum supported match length (in bytes)
pub(crate) const DEFLATE_MIN_MATCH_LEN: usize = 3;
/// The maximum supported match length (in bytes)
pub(crate) const DEFLATE_MAX_MATCH_LEN: usize = 258;

/// log2 of DEFLATE_MAX_MATCH_OFFSET
pub(crate) const DEFLATE_WINDOW_ORDER: u32 = 15;

// Number of symbols in each Huffman code. Note: for the literal/length and offset codes,
// these are actually the maximum values; a given block might use fewer symbols.
pub(crate) const DEFLATE_NUM_PRECODE_SYMS: usize = 19;
pub(crate) const DEFLATE_NUM_LITLEN_SYMS: usize = 288;
pub(crate) const DEFLATE_NUM_OFFSET_SYMS: usize = 32;

/// The maximum number of symbols across all codes
pub(crate) const DEFLATE_MAX_NUM_SYMS: usize = 288;

/// Division of symbols in the literal/length code
pub(crate) const DEFLATE_NUM_LITERALS: usize = 256;
pub(crate) const DEFLATE_END_OF_BLOCK: usize = 256;
pub(crate) const DEFLATE_FIRST_LEN_SYM: usize = 257;

// Maximum codeword length, in bits, within each Huffman code
pub(crate) const DEFLATE_MAX_PRE_CODEWORD_LEN: u32 = 7;
pub(crate) const DEFLATE_MAX_LITLEN_CODEWORD_LEN: u32 = 15;
pub(crate) const DEFLATE_MAX_OFFSET_CODEWORD_LEN: u32 = 15;

/// The maximum codeword length across all codes
pub(crate) const DEFLATE_MAX_CODEWORD_LEN: usize = 15;

/// Maximum possible overrun when decoding codeword lengths
pub(crate) const DEFLATE_MAX_LENS_OVERRUN: usize = 137;

// Maximum number of extra bits that may be required to represent a match length or offset.
pub(crate) const DEFLATE_MAX_EXTRA_LENGTH_BITS: u32 = 5;
pub(crate) const DEFLATE_MAX_EXTRA_OFFSET_BITS: u32 = 13;
