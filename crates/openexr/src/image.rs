// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! A whole file in memory: every part's header and channels, as the wheel's `OpenEXR.File`
//! gives them (PyOpenEXR.cpp:386-470 @ v3.5.3), read through [`FrameBuffer`] and
//! [`DeepFrameBuffer`].

use std::path::Path;

use crate::attribute::{PixelType, Storage};
use crate::deep::DeepFrameBuffer;
use crate::error::Result;
use crate::frame_buffer::{FrameBuffer, sample_count};
use crate::header::Header;
use crate::input::MultiPartInputFile;
use crate::read::InputPart;

/// A channel's samples, in its own type (halves as their bits).
#[derive(Debug, Clone, PartialEq)]
pub enum Pixels {
    Half(Vec<u16>),
    Float(Vec<f32>),
    Uint(Vec<u32>),
}

impl Pixels {
    fn zeroed(t: PixelType, n: usize) -> Pixels {
        match t {
            PixelType::Half => Pixels::Half(vec![0; n]),
            PixelType::Float => Pixels::Float(vec![0.0; n]),
            _ => Pixels::Uint(vec![0; n]),
        }
    }

    /// The type.
    pub fn pixel_type(&self) -> PixelType {
        match self {
            Pixels::Half(_) => PixelType::Half,
            Pixels::Float(_) => PixelType::Float,
            Pixels::Uint(_) => PixelType::Uint,
        }
    }

    /// The number of samples.
    pub fn len(&self) -> usize {
        match self {
            Pixels::Half(v) => v.len(),
            Pixels::Float(v) => v.len(),
            Pixels::Uint(v) => v.len(),
        }
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The samples' bytes, in the platform's order.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Pixels::Half(v) => bytemuck::cast_slice(v),
            Pixels::Float(v) => bytemuck::cast_slice(v),
            Pixels::Uint(v) => bytemuck::cast_slice(v),
        }
    }
}

/// A channel of a part read whole.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageChannel {
    pub name: Vec<u8>,
    pub x_sampling: i32,
    pub y_sampling: i32,
    /// Samples per row (the data window's columns that are multiples of `x_sampling`).
    pub width: usize,
    /// Rows (the data window's lines that are multiples of `y_sampling`).
    pub height: usize,
    /// A flat channel's samples, row by row; a deep channel's, each pixel's after the
    /// previous pixel's (their numbers in [`ImagePart::sample_counts`]).
    pub pixels: Pixels,
}

/// A part read whole: its header and channels (in the header's order), and for a deep part
/// its pixels' sample counts, row by row.
#[derive(Debug, Clone, PartialEq)]
pub struct ImagePart {
    pub header: Header,
    pub channels: Vec<ImageChannel>,
    pub sample_counts: Option<Vec<u32>>,
}

/// Every part of a file, read whole (a tiled part's level 0, as the C++ library's
/// `InputFile` and the wheel read it).
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub parts: Vec<ImagePart>,
}

impl Image {
    /// Reads the file at `path`.
    pub fn read(path: impl AsRef<Path>) -> Result<Image> {
        Image::from_file(&MultiPartInputFile::open(path)?)
    }

    /// Reads every part of `file`. Where the wheel leaves out a part whose pixels it cannot
    /// read, this fails with the part's error.
    pub fn from_file(file: &MultiPartInputFile) -> Result<Image> {
        let parts = (0..file.parts())
            .map(|p| read_part(&file.part(p)?))
            .collect::<Result<_>>()?;
        Ok(Image { parts })
    }
}

fn read_part(part: &InputPart<'_>) -> Result<ImagePart> {
    let h = part.header();
    let dw = h.data_window();
    let storage = h.storage();
    let mut channels: Vec<ImageChannel> = h
        .channels()
        .iter()
        .map(|c| ImageChannel {
            name: c.name.clone(),
            x_sampling: c.x_sampling,
            y_sampling: c.y_sampling,
            width: sample_count(dw.min.x, dw.max.x, c.x_sampling),
            height: sample_count(dw.min.y, dw.max.y, c.y_sampling),
            pixels: Pixels::zeroed(c.pixel_type, 0),
        })
        .collect();
    let mut sample_counts = None;
    if storage.is_deep() {
        let mut counts = vec![0u32; (dw.width() * dw.height()) as usize];
        let tiles = if storage == Storage::DeepTiled {
            Some(part.num_tiles((0, 0))?)
        } else {
            None
        };
        match tiles {
            Some((nx, ny)) => {
                part.read_tile_sample_counts(&mut counts, dw, (0, nx - 1), (0, ny - 1), (0, 0))?
            }
            None => part.read_sample_counts(&mut counts, dw, dw.min.y, dw.max.y)?,
        }
        let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
        for (ch, c) in channels.iter_mut().zip(h.channels()) {
            ch.pixels = Pixels::zeroed(c.pixel_type, total as usize);
        }
        let mut fb = DeepFrameBuffer::new(&counts, dw)?;
        for ch in channels.iter_mut() {
            match &mut ch.pixels {
                Pixels::Half(v) => fb.insert(&ch.name, v)?,
                Pixels::Float(v) => fb.insert(&ch.name, v)?,
                Pixels::Uint(v) => fb.insert(&ch.name, v)?,
            };
        }
        match tiles {
            Some((nx, ny)) => part.read_deep_tiles(&mut fb, (0, nx - 1), (0, ny - 1), (0, 0))?,
            None => part.read_deep_pixels(&mut fb, dw.min.y, dw.max.y)?,
        }
        drop(fb);
        sample_counts = Some(counts);
    } else {
        for (ch, c) in channels.iter_mut().zip(h.channels()) {
            ch.pixels = Pixels::zeroed(c.pixel_type, ch.width * ch.height);
        }
        let mut fb = FrameBuffer::new();
        for ch in channels.iter_mut() {
            let (xs, ys) = (ch.x_sampling, ch.y_sampling);
            match &mut ch.pixels {
                Pixels::Half(v) => fb.insert_sampled_plane(&ch.name, v, dw, xs, ys)?,
                Pixels::Float(v) => fb.insert_sampled_plane(&ch.name, v, dw, xs, ys)?,
                Pixels::Uint(v) => fb.insert_sampled_plane(&ch.name, v, dw, xs, ys)?,
            };
        }
        part.read_pixels(&mut fb, dw.min.y, dw.max.y)?;
    }
    Ok(ImagePart {
        header: h.clone(),
        channels,
        sample_counts,
    })
}
