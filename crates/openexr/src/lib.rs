// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Read and write OpenEXR files, safely, over an exact Rust port of OpenEXRCore 3.5.3
//! (`openexr-rs-core`).
//!
//! Every pixel and byte comes from the Core port, which matches the official OpenEXR 3.5.3
//! byte for byte; the files this crate writes are the C++ library's, byte for byte. The names
//! follow the C++ library's (`Header`, `MultiPartInputFile`, `FrameBuffer`, `Slice`, ...) in
//! Rust's case. There is no `unsafe` in the crate.
//!
//! # A whole file at once
//!
//! [`Image::read`] gives every part's header and channels as owned planes, as the Python
//! binding's `OpenEXR.File` does; [`Image::write`] writes them back.
//!
//! ```
//! use openexr::{Channel, Compression, FrameBuffer, Header, Image, OutputFile, PixelType};
//! # use std::io::Cursor;
//! # let window = openexr::Box2i { min: openexr::V2 { x: 0, y: 0 }, max: openexr::V2 { x: 3, y: 1 } };
//! # let mut h = Header::new(window);
//! # h.insert_channel(Channel::new("Y", PixelType::Float));
//! # let mut fb = FrameBuffer::new();
//! # let y = [0.5f32; 8];
//! # fb.insert_plane("Y", &y, window)?;
//! # let mut out = openexr::WriteOptions::new().create_writer("m", Cursor::new(Vec::new()), &[h])?;
//! # out.part(0)?.write_pixels(&fb, 2)?;
//! # let bytes = out.finish()?.into_inner();
//! # let file = openexr::ReadOptions::new().open_bytes("image.exr", bytes)?;
//! // let image = Image::read("image.exr")?;
//! let image = Image::from_file(&file)?;
//! for part in &image.parts {
//!     let dw = part.header.data_window();
//!     for ch in &part.channels {
//!         println!("{}: {} by {}, {:?}", String::from_utf8_lossy(&ch.name), ch.width, ch.height,
//!                  ch.pixels.pixel_type());
//!     }
//! #   assert_eq!((dw.width(), dw.height()), (4, 2));
//! }
//! # assert_eq!(image.parts[0].channels[0].pixels, openexr::Pixels::Float(vec![0.5; 8]));
//! # Ok::<(), openexr::Error>(())
//! ```
//!
//! # Into and out of your own buffers
//!
//! A [`FrameBuffer`] maps channel names to [`Slice`]s of your buffers (`&mut [f32]`,
//! `&mut [u16]` halves, `&mut [u32]`, or bytes; `&` ones to write from), with any strides:
//! planes, interleaved pixels, subsampled channels. Every slice is checked to fit its buffer
//! before Core is called; a channel the file lacks gets its slice's fill value; a read may ask
//! for another pixel type than the file's, which Core converts.
//!
//! ```
//! use openexr::{Box2i, Channel, Compression, FrameBuffer, Header, PixelType, ReadOptions, V2,
//!               WriteOptions};
//! use std::io::Cursor;
//!
//! let window = Box2i { min: V2 { x: 0, y: 0 }, max: V2 { x: 63, y: 31 } };
//! let rgb = vec![0.25f32; 64 * 32 * 3];
//!
//! // write three interleaved float channels as a ZIP-compressed scanline file
//! let mut header = Header::new(window);
//! for c in ["R", "G", "B"] {
//!     header.insert_channel(Channel::new(c, PixelType::Float));
//! }
//! header.set_compression(Compression::Zip);
//! let mut fb = FrameBuffer::new();
//! fb.insert_interleaved(&["R", "G", "B"], &rgb, window)?;
//! let mut out = WriteOptions::new().create_writer("rgb.exr", Cursor::new(Vec::new()),
//!                                                 &[header])?;
//! out.part(0)?.write_pixels(&fb, 32)?;
//! let bytes = out.finish()?.into_inner();
//!
//! // read the green channel back as halves, and lines 8 to 15 only
//! let file = ReadOptions::new().open_bytes("rgb.exr", bytes)?;
//! let mut green = vec![0u16; 64 * 32];
//! let mut fb = FrameBuffer::new();
//! fb.insert_plane("G", &mut green, window)?;
//! file.part(0)?.read_pixels(&mut fb, 8, 15)?;
//! drop(fb);
//! assert_eq!(openexr::half::to_f32(green[8 * 64]), 0.25);
//! assert_eq!(green[0], 0); // line 0 was not read
//! # Ok::<(), openexr::Error>(())
//! ```
//!
//! Tiled parts are read and written by tile ([`InputPart::read_tiles`],
//! [`OutputPart::write_tiles`]) at any level, or by line; deep parts through a
//! [`DeepFrameBuffer`]: their sample counts first, then the samples, into one buffer per
//! channel sized from the counts.
//!
//! # Threads
//!
//! Reads and writes run on the calling thread unless a file asks for more
//! ([`ReadOptions::threads`], [`WriteOptions::threads`]): then a read decodes its chunks in
//! parallel straight into your buffers, and a write compresses its chunks in parallel; the
//! bytes are the same. [`Threads::Auto`] uses the current `rayon` pool, your own when called
//! from inside its `install`. Without the default `rayon` feature, every request runs
//! single-threaded.
//!
//! # Errors
//!
//! Every call returns a [`Result`]; an [`Error`] carries Core's error code and Core's message,
//! word for word.
//!
//! # Files you do not trust
//!
//! A file's header sizes the buffers a read makes. Nothing a file holds makes the crate panic
//! or abort (a fuzzer of every read and write path runs on every full check), and a buffer
//! too large to allocate is an [`ErrorCode::OutOfMemory`] error; but a header may ask for a
//! window large enough to be allocated and still be far more than you meant to spend. Set
//! [`ReadOptions::max_image_size`] and [`ReadOptions::max_tile_size`] for files from sources
//! you do not trust.

#![forbid(unsafe_code)]

pub mod attribute;
mod deep;
mod error;
mod frame_buffer;
pub mod half;
mod header;
mod image;
mod input;
mod output;
mod parallel;
mod read;

pub use attribute::{
    Attribute, Box2, Box2f, Box2i, Channel, Chromaticities, Compression, DeepImageState, Envmap,
    KeyCode, LevelMode, LineOrder, PixelType, Preview, Rational, RoundingMode, Storage,
    TileDescription, TimeCode, V2, V2d, V2f, V2i, V3, V3d, V3f, V3i,
};
pub use deep::{DeepFrameBuffer, DeepSlice};
pub use error::{Error, ErrorCode, Result};
pub use frame_buffer::{FrameBuffer, Sample, SampleData, Slice};
pub use header::Header;
pub use image::{Image, ImageChannel, ImagePart, Pixels};
pub use input::{InputFile, MultiPartInputFile, ReadOptions};
pub use output::{MultiPartOutputFile, OutputFile, OutputPart, WriteOptions};
pub use parallel::Threads;
pub use read::InputPart;
