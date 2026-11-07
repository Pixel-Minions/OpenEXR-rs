// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! An exact Rust port of OpenEXRCore 3.5.3 (`src/lib/OpenEXRCore` @ v3.5.3).
//!
//! Each module mirrors one upstream file and each function cites the upstream lines it
//! translates. `PLAN.md` is the source of truth for scope and order.

pub mod base;
pub mod bytes;
#[doc(hidden)]
pub mod cfmt;
pub mod channel_list;
pub mod context;
pub mod errors;
pub mod float_vector;
mod imath_half;
mod internal_coding;
mod internal_constants;
mod internal_cpuid;
mod internal_ht_quality;
mod internal_xdr;
pub mod opaque;
pub mod openexr_attr;
pub mod preview;
pub mod string;
pub mod string_vector;
pub mod version;

#[cfg(test)]
mod upstream_tests;

pub use base::*;
pub use context::{Context, Error, ErrorHandler, ErrorReport, Initializer};
pub use errors::{ErrorCode, ExrResult};
