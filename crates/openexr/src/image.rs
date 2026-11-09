// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! A whole file in memory: every part's header and channels, as the wheel's `OpenEXR.File`
//! gives them (PyOpenEXR.cpp:386-470 @ v3.5.3), read through [`FrameBuffer`] and
//! [`DeepFrameBuffer`].

use std::path::Path;

use crate::attribute::{PixelType, Storage};
use crate::deep::DeepFrameBuffer;
use crate::error::{Error, ErrorCode, Result, alloc, product};
use crate::frame_buffer::{FrameBuffer, Slice, sample_count};
use crate::header::Header;
use crate::input::MultiPartInputFile;
use crate::output::{MultiPartOutputFile, WriteOptions};
use crate::read::InputPart;

/// A channel's samples, in its own type (halves as their bits).
#[derive(Debug, Clone, PartialEq)]
pub enum Pixels {
    Half(Vec<u16>),
    Float(Vec<f32>),
    Uint(Vec<u32>),
}

impl Pixels {
    fn zeroed(t: PixelType, n: usize, what: &str) -> Result<Pixels> {
        Ok(match t {
            PixelType::Half => Pixels::Half(alloc(n, what)?),
            PixelType::Float => Pixels::Float(alloc(n, what)?),
            _ => Pixels::Uint(alloc(n, what)?),
        })
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

    /// Writes the image to the file at `path` ([`write_to`](Self::write_to)).
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        let headers: Vec<Header> = self.parts.iter().map(|p| p.header.clone()).collect();
        let mut out = WriteOptions::new().create(path, &headers)?;
        self.write_parts(&mut out)?;
        out.finish().map(drop)
    }

    /// Writes the image to `writer` with `options`, as the wheel's `OpenEXR.File.write`
    /// writes it (PyOpenEXR.cpp:891-987, 1121-1200 @ v3.5.3): a scanline part's lines in one
    /// call, a tiled part's level 0 tiles in one call, a deep part's lines in one call or
    /// its tiles one at a time, row by row; and gives the writer back. `name` is the file
    /// name Core's messages give. An image holds level 0 alone: a part of several levels
    /// is refused (the C++ library would leave its other levels unwritten), and is written
    /// through [`OutputPart`](crate::OutputPart) instead.
    pub fn write_to<W: std::io::Write + std::io::Seek + Send + 'static>(
        &self,
        name: &str,
        writer: W,
        options: &WriteOptions,
    ) -> Result<W> {
        let headers: Vec<Header> = self.parts.iter().map(|p| p.header.clone()).collect();
        let mut out = options.create_writer(name, writer, &headers)?;
        self.write_parts(&mut out)?;
        out.finish()
    }

    fn write_parts<W: std::io::Write + std::io::Seek + Send + 'static>(
        &self,
        out: &mut MultiPartOutputFile<W>,
    ) -> Result<()> {
        for (p, ip) in self.parts.iter().enumerate() {
            let h = &ip.header;
            let dw = h.data_window();
            let mut part = out.part(p)?;
            if h.storage().is_deep() {
                let counts = ip.sample_counts.as_deref().ok_or_else(|| {
                    Error::new(
                        ErrorCode::InvalidArgument,
                        "A deep part needs its sample counts",
                    )
                })?;
                let mut fb = DeepFrameBuffer::new(counts, dw)?;
                for ch in &ip.channels {
                    fb.insert_bytes(&ch.name, ch.pixels.pixel_type(), ch.pixels.as_bytes())?;
                }
                if h.storage() == Storage::DeepTiled {
                    let (nx, ny) = part.num_tiles((0, 0))?;
                    for ty in 0..ny {
                        for tx in 0..nx {
                            part.write_deep_tile(&fb, tx, ty, (0, 0))?;
                        }
                    }
                } else {
                    part.write_deep_pixels(&fb, dw.height() as i32)?;
                }
                continue;
            }
            let mut fb = FrameBuffer::new();
            for ch in &ip.channels {
                let size = ch.pixels.pixel_type().size() as i64;
                let id = fb.add_bytes(ch.pixels.as_bytes());
                let mut s = Slice::new(
                    ch.pixels.pixel_type(),
                    id,
                    0,
                    dw.min,
                    size,
                    size * ch.width as i64,
                );
                s.x_sampling = ch.x_sampling;
                s.y_sampling = ch.y_sampling;
                fb.insert(&ch.name, s)?;
            }
            if h.storage() == Storage::Tiled {
                let (nx, ny) = part.num_tiles((0, 0))?;
                part.write_tiles(&fb, (0, nx - 1), (0, ny - 1), (0, 0))?;
            } else {
                part.write_pixels(&fb, dw.height() as i32)?;
            }
        }
        Ok(())
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
            pixels: Pixels::Half(Vec::new()),
        })
        .collect();
    let mut sample_counts = None;
    if storage.is_deep() {
        let n = product(dw.width(), dw.height(), 1, "a deep part's sample counts")?;
        let mut counts: Vec<u32> = alloc(n, "a deep part's sample counts")?;
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
            let n = usize::try_from(total).map_err(|_| {
                Error::new(
                    ErrorCode::OutOfMemory,
                    "A deep channel's samples are too many",
                )
            })?;
            ch.pixels = Pixels::zeroed(c.pixel_type, n, "a deep channel's samples")?;
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
            let n = product(ch.width as u64, ch.height as u64, 1, "a channel's samples")?;
            ch.pixels = Pixels::zeroed(c.pixel_type, n, "a channel's samples")?;
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
