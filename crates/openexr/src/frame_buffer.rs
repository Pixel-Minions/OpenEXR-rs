// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The caller's buffers, by channel name: what the C++ library's `FrameBuffer` and `Slice`
//! are, in safe Rust.
//!
//! A [`FrameBuffer`] borrows the caller's buffers (any of `u16` halves, `f32` or `u32`, or
//! bytes) and maps channel names to [`Slice`]s, each of which says where in one of those
//! buffers each pixel of the channel goes. Several slices can share a buffer (interleaved
//! pixels), which C++ does with pointers and Rust does with byte offsets into the buffer.
//! A sample's place is the C++ library's (`Slice::Make`, src/lib/OpenEXR/ImfFrameBuffer.cpp:
//! 46-96 @ v3.5.3): `(x / x_sampling) * x_stride + (y / y_sampling) * y_stride` from the
//! slice's base, C's integer division truncating toward zero.

use std::collections::BTreeMap;

use crate::attribute::{Box2i, PixelType, V2i};
use crate::error::{Error, ErrorCode, Result};

mod sealed {
    pub trait Sealed {}
    impl Sealed for u16 {}
    impl Sealed for u32 {}
    impl Sealed for f32 {}
}

/// A sample type a buffer can hold: `u16` (a half's bits), `f32` or `u32`.
pub trait Sample: bytemuck::Pod + sealed::Sealed {
    /// The pixel type of a buffer of this type.
    const PIXEL_TYPE: PixelType;
}

impl Sample for u16 {
    const PIXEL_TYPE: PixelType = PixelType::Half;
}

impl Sample for f32 {
    const PIXEL_TYPE: PixelType = PixelType::Float;
}

impl Sample for u32 {
    const PIXEL_TYPE: PixelType = PixelType::Uint;
}

/// Where a channel's pixels go: a buffer of the [`FrameBuffer`], the pixel type to store,
/// the strides and the sampling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    /// The type stored; Core converts from the file's.
    pub pixel_type: PixelType,
    /// The buffer, as [`FrameBuffer::add_buffer`] numbered it.
    pub buffer: usize,
    /// The byte offset, in the buffer, of the sample of pixel `origin`.
    pub offset: usize,
    /// The pixel whose sample is at `offset`.
    pub origin: V2i,
    /// Bytes from a sample to the next in x.
    pub x_stride: i64,
    /// Bytes from a sample to the next in y.
    pub y_stride: i64,
    pub x_sampling: i32,
    pub y_sampling: i32,
    /// The value stored for a channel the file lacks (converted as the C++ library does:
    /// `float (v)`, `half (v)`; for `uint`, truncated toward zero, saturating).
    pub fill_value: f64,
}

impl Slice {
    /// A slice of `pixel_type` samples in `buffer`, the sample of pixel `origin` at byte
    /// `offset`, with the given strides, sampling 1 and fill value 0.
    pub fn new(
        pixel_type: PixelType,
        buffer: usize,
        offset: usize,
        origin: V2i,
        x_stride: i64,
        y_stride: i64,
    ) -> Slice {
        Slice {
            pixel_type,
            buffer,
            offset,
            origin,
            x_stride,
            y_stride,
            x_sampling: 1,
            y_sampling: 1,
            fill_value: 0.0,
        }
    }

    /// The byte offset of pixel `(x, y)`'s sample (C++'s `base + (x / xs) * xStride +
    /// (y / ys) * yStride`, its base `ptr - (origin.x / xs) * xStride - (origin.y / ys) *
    /// yStride`), or `None` if it overflows `i64`.
    pub fn offset_of(&self, x: i32, y: i32) -> Option<i64> {
        let xs = i64::from(self.x_sampling);
        let ys = i64::from(self.y_sampling);
        let dx = (i64::from(x) / xs).checked_sub(i64::from(self.origin.x) / xs)?;
        let dy = (i64::from(y) / ys).checked_sub(i64::from(self.origin.y) / ys)?;
        i64::try_from(self.offset)
            .ok()?
            .checked_add(dx.checked_mul(self.x_stride)?)?
            .checked_add(dy.checked_mul(self.y_stride)?)
    }

    /// The bytes one sample takes.
    pub fn sample_size(&self) -> usize {
        self.pixel_type.size()
    }
}

/// How many of the coordinates `lo..=hi` are multiples of `s` (a sampled channel's samples
/// along that axis).
pub(crate) fn sample_count(lo: i32, hi: i32, s: i32) -> usize {
    if hi < lo || s < 1 {
        return 0;
    }
    let first = first_multiple(lo, s);
    if first > i64::from(hi) {
        return 0;
    }
    ((i64::from(hi) - first) / i64::from(s) + 1) as usize
}

/// The first multiple of `s` at or after `lo`.
pub(crate) fn first_multiple(lo: i32, s: i32) -> i64 {
    let (lo, s) = (i64::from(lo), i64::from(s));
    lo + (s - lo.rem_euclid(s)) % s
}

/// The caller's buffers and, by channel name, the slices into them.
#[derive(Debug, Default)]
pub struct FrameBuffer<'a> {
    buffers: Vec<&'a mut [u8]>,
    slices: BTreeMap<Vec<u8>, Slice>,
}

impl<'a> FrameBuffer<'a> {
    /// An empty frame buffer.
    pub fn new() -> FrameBuffer<'a> {
        FrameBuffer::default()
    }

    /// Adds a buffer of samples; returns its number, for [`Slice::buffer`].
    pub fn add_buffer<T: Sample>(&mut self, data: &'a mut [T]) -> usize {
        self.add_bytes(bytemuck::cast_slice_mut(data))
    }

    /// Adds a buffer of bytes; returns its number, for [`Slice::buffer`].
    pub fn add_bytes(&mut self, data: &'a mut [u8]) -> usize {
        self.buffers.push(data);
        self.buffers.len() - 1
    }

    /// Sets the slice of channel `name`, replacing any it had. Refuses a slice whose buffer
    /// does not exist, whose type is unknown or whose sampling is below 1.
    pub fn insert(&mut self, name: impl AsRef<[u8]>, slice: Slice) -> Result<&mut Slice> {
        if slice.buffer >= self.buffers.len() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                format!(
                    "Slice refers to buffer {}, which does not exist",
                    slice.buffer
                ),
            ));
        }
        if matches!(slice.pixel_type, PixelType::Other(_)) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Invalid pixel type.",
            ));
        }
        if slice.x_sampling < 1 || slice.y_sampling < 1 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Slice sampling factors must be at least 1",
            ));
        }
        let name = name.as_ref().to_vec();
        self.slices.insert(name.clone(), slice);
        Ok(self.slices.get_mut(&name).expect("just inserted"))
    }

    /// Adds `data` as channel `name`'s plane over `window`: one sample per pixel, row by
    /// row, the first at `window.min`. Refuses `data` smaller than the window.
    pub fn insert_plane<T: Sample>(
        &mut self,
        name: impl AsRef<[u8]>,
        data: &'a mut [T],
        window: Box2i,
    ) -> Result<&mut Slice> {
        self.insert_sampled_plane(name, data, window, 1, 1)
    }

    /// Adds `data` as channel `name`'s plane over `window`, sampled every `x_sampling`
    /// columns and `y_sampling` rows: one sample per sampled pixel, row by row. Refuses
    /// `data` smaller than the window's samples.
    pub fn insert_sampled_plane<T: Sample>(
        &mut self,
        name: impl AsRef<[u8]>,
        data: &'a mut [T],
        window: Box2i,
        x_sampling: i32,
        y_sampling: i32,
    ) -> Result<&mut Slice> {
        if x_sampling < 1 || y_sampling < 1 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Slice sampling factors must be at least 1",
            ));
        }
        let w = sample_count(window.min.x, window.max.x, x_sampling);
        let h = sample_count(window.min.y, window.max.y, y_sampling);
        check_len(data.len(), w * h)?;
        let size = std::mem::size_of::<T>() as i64;
        let buffer = self.add_buffer(data);
        let mut slice = Slice::new(T::PIXEL_TYPE, buffer, 0, window.min, size, size * w as i64);
        slice.x_sampling = x_sampling;
        slice.y_sampling = y_sampling;
        self.insert(name, slice)
    }

    /// Adds `data` as the pixels of `names` over `window`, interleaved: each pixel holds one
    /// sample of each channel, in the order of `names`, and pixels follow row by row. Refuses
    /// `data` smaller than the window's pixels.
    pub fn insert_interleaved<T: Sample, N: AsRef<[u8]>>(
        &mut self,
        names: &[N],
        data: &'a mut [T],
        window: Box2i,
    ) -> Result<()> {
        let w = sample_count(window.min.x, window.max.x, 1);
        let h = sample_count(window.min.y, window.max.y, 1);
        check_len(data.len(), w * h * names.len())?;
        let size = std::mem::size_of::<T>();
        let pixel = (size * names.len()) as i64;
        let buffer = self.add_buffer(data);
        for (k, name) in names.iter().enumerate() {
            let slice = Slice::new(
                T::PIXEL_TYPE,
                buffer,
                k * size,
                window.min,
                pixel,
                pixel * w as i64,
            );
            self.insert(name, slice)?;
        }
        Ok(())
    }

    /// The slice of channel `name`.
    pub fn slice(&self, name: impl AsRef<[u8]>) -> Option<&Slice> {
        self.slices.get(name.as_ref())
    }

    /// The slice of channel `name`, to change.
    pub fn slice_mut(&mut self, name: impl AsRef<[u8]>) -> Option<&mut Slice> {
        self.slices.get_mut(name.as_ref())
    }

    /// The slices, by name.
    pub fn slices(&self) -> impl Iterator<Item = (&[u8], &Slice)> {
        self.slices.iter().map(|(n, s)| (n.as_slice(), s))
    }

    pub(crate) fn buffer_len(&self, buffer: usize) -> usize {
        self.buffers[buffer].len()
    }

    pub(crate) fn buffers_mut(&mut self) -> &mut [&'a mut [u8]] {
        &mut self.buffers
    }
}

fn check_len(have: usize, need: usize) -> Result<()> {
    if have < need {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            format!("Buffer holds {have} samples, the window needs {need}"),
        ));
    }
    Ok(())
}
