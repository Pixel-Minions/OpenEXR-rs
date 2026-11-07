// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Port of `zlib_constants.h` (external/deflate/lib/zlib_constants.h:1-21 @ libdeflate v1.26):
//! constants for the zlib wrapper format.

pub(crate) const ZLIB_MIN_HEADER_SIZE: usize = 2;
pub(crate) const ZLIB_FOOTER_SIZE: usize = 4;
pub(crate) const ZLIB_MIN_OVERHEAD: usize = ZLIB_MIN_HEADER_SIZE + ZLIB_FOOTER_SIZE;

pub(crate) const ZLIB_CM_DEFLATE: u16 = 8;

pub(crate) const ZLIB_CINFO_32K_WINDOW: u16 = 7;

pub(crate) const ZLIB_FASTEST_COMPRESSION: u16 = 0;
pub(crate) const ZLIB_FAST_COMPRESSION: u16 = 1;
pub(crate) const ZLIB_DEFAULT_COMPRESSION: u16 = 2;
pub(crate) const ZLIB_SLOWEST_COMPRESSION: u16 = 3;
