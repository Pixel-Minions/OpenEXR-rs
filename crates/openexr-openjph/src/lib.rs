// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! An exact port of OpenJPH 0.32.0 (`external/OpenJPH` @ v3.5.3 of OpenEXR), the HTJ2K
//! codec OpenEXRCore uses for HTJ2K32, HTJ2K256 and LJ2K. In progress: its generic (portable)
//! code first, from the leaves; the SIMD kernels it chooses at run time follow.

#[allow(dead_code)] // used as the port grows
mod arch;
mod base;
#[allow(dead_code)] // used as the port grows
mod block_common;
#[allow(dead_code)] // used as the port grows
mod block_decoder32;
#[allow(dead_code)] // used as the port grows
mod block_decoder64;
#[allow(dead_code)] // used as the port grows
mod file;
#[allow(dead_code)] // used as the port grows
mod message;
#[allow(dead_code)] // used as the port grows
mod params;

pub use base::{Point, Rect, Size};
pub use file::{Infile, MemInfile, Outfile, Seek};
pub use message::{OjphError, OjphResult};
