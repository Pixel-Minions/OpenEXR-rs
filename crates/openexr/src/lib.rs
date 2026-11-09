// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Read and write OpenEXR files, safely, over an exact Rust port of OpenEXRCore 3.5.3
//! (`openexr-rs-core`).
//!
//! The names follow the C++ library's (`Header`, `MultiPartInputFile`, `InputFile`, ...) in
//! Rust's case; every pixel and byte comes from the Core port, which matches the official
//! OpenEXR 3.5.3 byte for byte (PLAN.md §16).
//!
//! ```no_run
//! let file = openexr::MultiPartInputFile::open("image.exr")?;
//! for header in file.headers() {
//!     let dw = header.data_window();
//!     println!("{}x{}, {:?}", dw.width(), dw.height(), header.compression());
//!     for c in header.channels() {
//!         println!("  {}: {:?}", String::from_utf8_lossy(&c.name), c.pixel_type);
//!     }
//! }
//! # Ok::<(), openexr::Error>(())
//! ```

#![forbid(unsafe_code)]

pub mod attribute;
mod error;
mod header;
mod input;

pub use attribute::{
    Attribute, Box2, Box2f, Box2i, Channel, Chromaticities, Compression, DeepImageState, Envmap,
    KeyCode, LevelMode, LineOrder, PixelType, Preview, Rational, RoundingMode, Storage,
    TileDescription, TimeCode, V2, V2d, V2f, V2i, V3, V3d, V3f, V3i,
};
pub use error::{Error, ErrorCode, Result};
pub use header::Header;
pub use input::{InputFile, MultiPartInputFile, ReadOptions};
