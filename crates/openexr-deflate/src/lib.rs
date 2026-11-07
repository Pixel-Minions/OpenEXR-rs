// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! An exact port of libdeflate v1.26 (`external/deflate` @ v3.5.3 of OpenEXR), the deflate
//! library OpenEXRCore uses for ZIP, ZIPS, PXR24 and DWA, so far its decompressor:
//! `libdeflate_deflate_decompress_ex`, `libdeflate_zlib_decompress_ex` and
//! `libdeflate_adler32`. The compressor follows with ZIP's compression side.
//!
//! The portable code is ported. libdeflate's x86 variants (the BMI2 build of the decompressor,
//! the vectorized Adler-32) compute the same results with other instructions; they are
//! performance work for later (PLAN.md O3). Custom allocators and the options struct version
//! check are n/a (PLAN.md O8): a decompressor is an ordinary Rust value.

mod adler32;
mod deflate_constants;
mod deflate_decompress;
mod zlib_decompress;

pub use adler32::adler32;
pub use deflate_decompress::Decompressor;

/// Port of `enum libdeflate_result` (external/deflate/libdeflate.h:194-209 @ libdeflate v1.26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum LibdeflateResult {
    /// Decompression was successful.
    Success = 0,
    /// Decompression failed because the compressed data was invalid, corrupt, or otherwise
    /// unsupported.
    BadData = 1,
    /// No actual output size was asked for, but the data would have decompressed to fewer
    /// than the output buffer's bytes.
    ShortOutput = 2,
    /// The data would have decompressed to more than the output buffer's bytes.
    InsufficientSpace = 3,
}
