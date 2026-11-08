// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! An exact Rust port of OpenEXRCore 3.5.3 (`src/lib/OpenEXRCore` @ v3.5.3).
//!
//! Each module mirrors one upstream file and each function cites the upstream lines it
//! translates. `PLAN.md` is the source of truth for scope and order.

pub mod attributes;
pub mod base;
mod buffer_pool;
pub mod bytes;
#[doc(hidden)]
pub mod cfmt;
pub mod channel_list;
pub mod chunk;
pub mod chunk_info;
pub mod chunk_write;
pub mod coding;
pub mod color_metadata;
pub mod compression;
pub mod context;
pub mod debug;
pub mod decoding;
pub mod encoding;
pub mod errors;
mod f16c_simd;
mod file_impl;
pub mod float_vector;
mod imath_half;
mod internal_b44;
mod internal_b44_table;
mod internal_coding;
mod internal_constants;
mod internal_cpuid;
mod internal_dwa;
#[cfg(target_arch = "x86_64")]
mod internal_dwa_avx;
mod internal_dwa_channeldata;
mod internal_dwa_classifier;
mod internal_dwa_compressor;
mod internal_dwa_decoder;
mod internal_dwa_encoder;
mod internal_dwa_helpers;
mod internal_dwa_simd;
mod internal_dwa_table;
mod internal_file;
mod internal_ht;
mod internal_ht_common;
mod internal_ht_quality;
mod internal_huf;
mod internal_piz;
mod internal_pxr24;
mod internal_rle;
mod internal_util;
mod internal_xdr;
mod internal_zip;
mod internal_zip_simd;
mod internal_zstd;
mod internal_zstd_delta;
mod internal_zstd_shuffle;
pub mod opaque;
pub mod openexr_attr;
mod os_error;
mod pack;
mod parse_header;
pub mod part;
pub mod part_attr;
pub mod part_attr_copy;
pub mod part_attr_set;
pub mod preview;
pub mod string;
pub mod string_vector;
pub mod unpack;
mod validation;
pub mod version;
mod write_header;

#[cfg(test)]
mod upstream_tests;

pub use base::*;
pub use context::{Context, DefaultWriteMode, Error, ErrorHandler, ErrorReport, Initializer};
pub use errors::{ErrorCode, ExrResult};
