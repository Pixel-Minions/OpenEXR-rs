// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! An exact port of OpenJPH 0.32.0 (`external/OpenJPH` @ v3.5.3 of OpenEXR), the HTJ2K
//! codec OpenEXRCore uses for HTJ2K32, HTJ2K256 and LJ2K. In progress: its generic (portable)
//! code first, from the leaves; the SIMD kernels it chooses at run time follow.

#[allow(dead_code)] // used as the port grows
mod arch;
mod base;
#[allow(dead_code)] // used as the port grows
mod bitbuffer_read;
#[allow(dead_code)] // used as the port grows
mod bitbuffer_write;
#[allow(dead_code)] // used as the port grows
mod block_common;
#[allow(dead_code)] // used as the port grows
mod block_decoder32;
#[allow(dead_code)] // used as the port grows
mod block_decoder64;
#[cfg(target_arch = "x86_64")]
mod block_decoder_avx2;
#[allow(dead_code)] // used as the port grows
mod block_encoder;
#[allow(dead_code)] // used as the port grows
mod codeblock;
#[allow(dead_code)] // used as the port grows
mod codeblock_fun;
#[allow(dead_code)] // used as the port grows
mod codestream;
#[allow(dead_code)] // used as the port grows
mod codestream_gen;
#[allow(dead_code)] // used as the port grows
mod colour;
#[allow(dead_code)] // used as the port grows
mod file;
#[allow(dead_code)] // used as the port grows
mod mem;
#[allow(dead_code)] // used as the port grows
mod message;
#[allow(dead_code)] // used as the port grows
mod params;
#[allow(dead_code)] // used as the port grows
mod precinct;
#[allow(dead_code)] // used as the port grows
mod resolution;
#[allow(dead_code)] // used as the port grows
mod subband;
#[allow(dead_code)] // used as the port grows
mod tile;
#[allow(dead_code)] // used as the port grows
mod tile_comp;
#[allow(dead_code)] // used as the port grows
mod transform;

pub use base::{Point, Rect, Size};
pub use codestream::{Codestream, CommentExchange};
pub use file::{Infile, MemInfile, Outfile, Seek};
pub use message::{ErrorStream, OjphError, OjphResult, set_error_stream};
