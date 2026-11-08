// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! An exact port of libdeflate v1.26 (`external/deflate` @ v3.5.3 of OpenEXR), the deflate
//! library OpenEXRCore uses for ZIP, ZIPS, PXR24 and DWA: its decompressor
//! (`libdeflate_deflate_decompress_ex`, `libdeflate_zlib_decompress_ex`), `libdeflate_adler32`,
//! and its compressor at every level, 0 to 12, with its output bounds.
//!
//! The portable code is ported. libdeflate's x86 variants (the BMI2 build of the decompressor,
//! the vectorized Adler-32) compute the same results with other instructions; they are
//! performance work for later (PLAN.md O3). Custom allocators and the options struct version
//! check are n/a (PLAN.md O8): a decompressor is an ordinary Rust value.

mod adler32;
#[cfg(target_arch = "x86_64")]
mod adler32_vnni;
#[cfg(target_arch = "x86_64")]
mod adler32_x86;
mod bt_matchfinder;
#[cfg(target_arch = "x86_64")]
mod decompress_bmi2;
mod deflate_compress;
mod deflate_constants;
mod deflate_decompress;
mod hc_matchfinder;
mod ht_matchfinder;
mod matchfinder_common;
mod zlib_compress;
mod zlib_constants;
mod zlib_decompress;

pub use adler32::adler32;
pub use deflate_compress::{Compressor, deflate_compress_bound};
pub use deflate_decompress::Decompressor;
pub use zlib_compress::zlib_compress_bound;

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
