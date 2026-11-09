// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Reading a deep part: its sample counts, then its samples, into the caller's buffers (the
//! C++ library's `DeepScanLineInputFile`, `DeepTiledInputFile` and `DeepFrameBuffer`).
//!
//! Where C++ hands the reader a table of pointers per channel, one per pixel, the caller here
//! gives each channel one buffer, sized from the sample counts: a [`DeepFrameBuffer`] covers a
//! window of pixels, row by row, and each pixel's samples follow the previous pixel's. The
//! pointer tables C++ builds by hand are built here from the counts, and Core decodes into
//! them as it does for C++ (`EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS`), each pixel's samples
//! checked to lie in their buffer.

use std::collections::BTreeMap;

use openexr_core::Context;
use openexr_core::chunk_info::ChunkInfo;
use openexr_core::coding::BufferRef;
use openexr_core::decoding::{
    DecodePipeline, EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS, EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL,
    EXR_DECODE_SAMPLE_DATA_ONLY,
};

use crate::attribute::{Box2i, PixelType, Storage};
use crate::error::{Error, ErrorCode, Result};
use crate::frame_buffer::Sample;
use crate::read::InputPart;

fn core(e: openexr_core::Error) -> Error {
    Error::from_core(&e)
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, msg)
}

/// The pixels of `window`, as a count.
fn pixels(window: &Box2i) -> usize {
    (window.width() * window.height()) as usize
}

/// The index of pixel `(x, y)` in `window`, row by row, if it is in it.
fn index(window: &Box2i, x: i32, y: i32) -> Option<usize> {
    let inside =
        (window.min.x..=window.max.x).contains(&x) && (window.min.y..=window.max.y).contains(&y);
    inside.then(|| {
        (y - window.min.y) as usize * window.width() as usize + (x - window.min.x) as usize
    })
}

/// A channel's samples: a buffer of one pixel type, each pixel's samples after the previous
/// pixel's.
#[derive(Debug)]
pub struct DeepSlice<'a> {
    pixel_type: PixelType,
    data: &'a mut [u8],
    /// The value stored in every sample of a channel the part lacks (converted as for a flat
    /// [`Slice`](crate::Slice)).
    pub fill_value: f64,
}

impl DeepSlice<'_> {
    /// The type stored; Core converts from the file's.
    pub fn pixel_type(&self) -> PixelType {
        self.pixel_type
    }
}

/// The buffers a deep read fills: the sample counts of a window of pixels, as
/// [`InputPart::read_sample_counts`] read them, and by channel name the samples' buffers.
#[derive(Debug)]
pub struct DeepFrameBuffer<'a> {
    window: Box2i,
    counts: &'a [u32],
    /// Each pixel's first sample, counted from the window's first, and the total at the end.
    starts: Vec<u64>,
    slices: BTreeMap<Vec<u8>, DeepSlice<'a>>,
}

impl<'a> DeepFrameBuffer<'a> {
    /// A frame buffer of the pixels of `window`, whose sample counts `counts` holds row by
    /// row. Refuses `counts` smaller than the window.
    pub fn new(counts: &'a [u32], window: Box2i) -> Result<DeepFrameBuffer<'a>> {
        let n = pixels(&window);
        if window.width() == 0 || window.height() == 0 || counts.len() < n {
            return Err(invalid(format!(
                "Sample counts hold {} pixels, the window needs {n}",
                counts.len()
            )));
        }
        let mut starts = Vec::with_capacity(n + 1);
        let mut acc = 0u64;
        for &c in &counts[..n] {
            starts.push(acc);
            acc += u64::from(c);
        }
        starts.push(acc);
        Ok(DeepFrameBuffer {
            window,
            counts,
            starts,
            slices: BTreeMap::new(),
        })
    }

    /// The window the frame buffer covers.
    pub fn window(&self) -> Box2i {
        self.window
    }

    /// The samples the window holds, the counts' sum: the length each channel's buffer needs.
    pub fn total_samples(&self) -> u64 {
        self.starts[self.starts.len() - 1]
    }

    /// The index of pixel `(x, y)`'s first sample in each channel's buffer.
    pub fn first_sample(&self, x: i32, y: i32) -> Option<u64> {
        index(&self.window, x, y).map(|i| self.starts[i])
    }

    /// Adds `data` as channel `name`'s samples. Refuses `data` shorter than
    /// [`total_samples`](Self::total_samples), or past the 4 GiB a deep buffer may hold.
    pub fn insert<T: Sample>(
        &mut self,
        name: impl AsRef<[u8]>,
        data: &'a mut [T],
    ) -> Result<&mut DeepSlice<'a>> {
        self.insert_bytes(name, T::PIXEL_TYPE, bytemuck::cast_slice_mut(data))
    }

    /// Adds `data`, samples of `pixel_type` as bytes, as channel `name`'s samples.
    pub fn insert_bytes(
        &mut self,
        name: impl AsRef<[u8]>,
        pixel_type: PixelType,
        data: &'a mut [u8],
    ) -> Result<&mut DeepSlice<'a>> {
        if matches!(pixel_type, PixelType::Other(_)) {
            return Err(invalid("Invalid pixel type."));
        }
        let need = self.total_samples() * pixel_type.size() as u64;
        if (data.len() as u64) < need {
            return Err(invalid(format!(
                "Buffer holds {} bytes, the samples need {need}",
                data.len()
            )));
        }
        // the pointers Core's port decodes to hold 32-bit offsets (BufferRef::to_pointer)
        if need > u64::from(u32::MAX) {
            return Err(invalid("A deep channel's samples must fit in 4 GiB"));
        }
        let name = name.as_ref().to_vec();
        self.slices.insert(
            name.clone(),
            DeepSlice {
                pixel_type,
                data,
                fill_value: 0.0,
            },
        );
        Ok(self.slices.get_mut(&name).expect("just inserted"))
    }

    /// The slice of channel `name`, to change.
    pub fn slice_mut(&mut self, name: impl AsRef<[u8]>) -> Option<&mut DeepSlice<'a>> {
        self.slices.get_mut(name.as_ref())
    }
}

/// The decoder of a deep read, kept from chunk to chunk as the C++ readers keep theirs.
struct Deep<'c> {
    dec: Option<DecodePipeline<'c>>,
}

/// A chunk's individual sample counts (with `EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL`): one
/// `int32_t` a pixel, row by row.
fn chunk_counts(d: &DecodePipeline<'_>, n: usize) -> Result<Vec<i32>> {
    let b = d.sample_counts();
    if b.len() < n * 4 {
        return Err(Error::new(
            ErrorCode::CorruptChunk,
            "Missing sample count table",
        ));
    }
    Ok(b[..n * 4]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| i32::from_ne_bytes(*c))
        .collect())
}

impl<'f> InputPart<'f> {
    fn deep_storage(&self, want: Storage) -> Result<()> {
        if self.header().storage() != want {
            return Err(invalid(match want {
                Storage::DeepScanline => "The part is not a deep scanline part",
                _ => "The part is not a deep tiled part",
            }));
        }
        Ok(())
    }

    /// The lines a deep scanline read covers, swapped into order and checked against the
    /// data window (`DeepScanLineInputFile::Data::readData`, src/lib/OpenEXR/
    /// ImfDeepScanLineInputFile.cpp:480-500 @ v3.5.3).
    fn deep_lines(&self, y1: i32, y2: i32, window: &Box2i) -> Result<(i32, i32)> {
        self.deep_storage(Storage::DeepScanline)?;
        let (y1, y2) = if y2 < y1 { (y2, y1) } else { (y1, y2) };
        let dw = self.header().data_window();
        if y1 < dw.min.y || y2 > dw.max.y {
            return Err(invalid(format!(
                "Tried to read scan line outside the image file's data window: {y1} - {y2} vs datawindow {} - {}",
                dw.min.y, dw.max.y
            )));
        }
        covers(window, dw.min.x, dw.max.x, y1, y2)?;
        Ok((y1, y2))
    }

    /// Reads the sample counts of lines `y1` to `y2` of a deep scanline part into `counts`,
    /// which holds those of `window` row by row (the rest are left as they are): port of
    /// `DeepScanLineInputFile::readPixelSampleCounts` (src/lib/OpenEXR/
    /// ImfDeepScanLineInputFile.cpp:406-416, 480-585 @ v3.5.3).
    pub fn read_sample_counts(
        &self,
        counts: &mut [u32],
        window: Box2i,
        y1: i32,
        y2: i32,
    ) -> Result<()> {
        check_counts(counts, &window)?;
        let (y1, y2) = self.deep_lines(y1, y2, &window)?;
        let mut deep = Deep { dec: None };
        self.for_each_line_chunk(y1, y2, |part, cinfo, at, skip, ignore| {
            part.deep_chunk(&mut deep, cinfo, at, skip, ignore, counts, &window, None)
        })
    }

    /// Reads the samples of lines `y1` to `y2` of a deep scanline part into `fb`, whose
    /// counts must be the part's (as [`read_sample_counts`](Self::read_sample_counts) read
    /// them): port of `DeepScanLineInputFile::readPixels` (src/lib/OpenEXR/
    /// ImfDeepScanLineInputFile.cpp:297-307, 480-585 @ v3.5.3).
    pub fn read_deep_pixels(&self, fb: &mut DeepFrameBuffer<'_>, y1: i32, y2: i32) -> Result<()> {
        let window = fb.window;
        let (y1, y2) = self.deep_lines(y1, y2, &window)?;
        self.check_deep_slices(fb)?;
        let mut deep = Deep { dec: None };
        let mut scratch = vec![0u32; 0];
        self.for_each_line_chunk(y1, y2, |part, cinfo, at, skip, ignore| {
            part.deep_chunk(
                &mut deep,
                cinfo,
                at,
                skip,
                ignore,
                &mut scratch,
                &window,
                Some(&mut *fb),
            )
        })
    }

    /// Reads the sample counts of tiles `dx` by `dy` of `level` of a deep tiled part into
    /// `counts`, which holds those of `window` (in the level's coordinates) row by row: port
    /// of `DeepTiledInputFile::readPixelSampleCounts` (src/lib/OpenEXR/
    /// ImfDeepTiledInputFile.cpp:666-760, 904-1004 @ v3.5.3).
    pub fn read_tile_sample_counts(
        &self,
        counts: &mut [u32],
        window: Box2i,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        check_counts(counts, &window)?;
        let mut deep = Deep { dec: None };
        self.for_each_deep_tile(dx, dy, level, &window, |part, cinfo, at| {
            part.deep_chunk(&mut deep, cinfo, at, 0, 0, counts, &window, None)
        })
    }

    /// Reads the samples of tiles `dx` by `dy` of `level` of a deep tiled part into `fb`:
    /// port of `DeepTiledInputFile::readTiles` (src/lib/OpenEXR/ImfDeepTiledInputFile.cpp:
    /// 316-356, 904-1004 @ v3.5.3).
    pub fn read_deep_tiles(
        &self,
        fb: &mut DeepFrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        let window = fb.window;
        self.check_deep_slices(fb)?;
        let mut deep = Deep { dec: None };
        let mut scratch = vec![0u32; 0];
        self.for_each_deep_tile(dx, dy, level, &window, |part, cinfo, at| {
            part.deep_chunk(
                &mut deep,
                cinfo,
                at,
                0,
                0,
                &mut scratch,
                &window,
                Some(&mut *fb),
            )
        })
    }

    /// Each channel the part has is sampled as its slice, 1 and 1 (`prepFillList`,
    /// src/lib/OpenEXR/ImfDeepScanLineInputFile.cpp:657-699 @ v3.5.3).
    fn check_deep_slices(&self, fb: &DeepFrameBuffer<'_>) -> Result<()> {
        for name in fb.slices.keys() {
            if let Some(c) = self.header().channel(name)
                && (c.x_sampling != 1 || c.y_sampling != 1)
            {
                return Err(invalid(format!(
                    "X and/or y subsampling factors of \"{}\" channel are not compatible with the frame buffer's subsampling factors.",
                    String::from_utf8_lossy(name)
                )));
            }
        }
        Ok(())
    }

    /// The chunks of lines `y1..=y2`, each with its first kept pixel and its line skips, as
    /// `DeepScanLineInputFile::Data::readData`'s loop walks them.
    fn for_each_line_chunk(
        &self,
        y1: i32,
        y2: i32,
        mut each: impl FnMut(&Self, &ChunkInfo, (i32, i32), i32, i32) -> Result<()>,
    ) -> Result<()> {
        let ctx = self.core_context();
        let p = self.index() as i32;
        let spc = ctx.get_scanlines_per_chunk(p).map_err(core)?;
        let mut y = y1;
        while y <= y2 {
            let cinfo = ctx.read_scanline_chunk_info(p, y).map_err(core)?;
            let end = i64::from(cinfo.start_y) + i64::from(cinfo.height) - 1;
            let ignore = if i64::from(y2) < end {
                (end - i64::from(y2)) as i32
            } else {
                0
            };
            each(self, &cinfo, (cinfo.start_x, y), y - cinfo.start_y, ignore)?;
            y += spc - (y - cinfo.start_y);
        }
        Ok(())
    }

    /// Tiles `dx` by `dy` of `level`, each with its first pixel in the level's coordinates,
    /// after checking the level and that the tiles lie in `window`.
    fn for_each_deep_tile(
        &self,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
        window: &Box2i,
        mut each: impl FnMut(&Self, &ChunkInfo, (i32, i32)) -> Result<()>,
    ) -> Result<()> {
        self.deep_storage(Storage::DeepTiled)?;
        if !self.is_valid_level(level.0, level.1) {
            return Err(invalid(format!(
                "Level coordinate ({}, {}) is invalid.",
                level.0, level.1
            )));
        }
        let dx = if dx.1 < dx.0 { (dx.1, dx.0) } else { dx };
        let dy = if dy.1 < dy.0 { (dy.1, dy.0) } else { dy };
        let first = self.data_window_for_tile(dx.0, dy.0, level)?;
        let last = self.data_window_for_tile(dx.1, dy.1, level)?;
        covers(window, first.min.x, last.max.x, first.min.y, last.max.y)?;
        let ctx = self.core_context();
        let p = self.index() as i32;
        let (tw, th) = self.tile_size(level)?;
        let dw = self.header().data_window();
        for ty in dy.0..=dy.1 {
            for tx in dx.0..=dx.1 {
                let cinfo = ctx
                    .read_tile_chunk_info(p, tx, ty, level.0, level.1)
                    .map_err(core)?;
                let at = (dw.min.x + tw * cinfo.start_x, dw.min.y + th * cinfo.start_y);
                each(self, &cinfo, at)?;
            }
        }
        Ok(())
    }

    /// Decodes one deep chunk: its sample counts into `counts` (without `fb`), or its
    /// samples into `fb`, the chunk's first kept pixel at `at`. Port of
    /// `ScanLineProcess::run_decode`, `update_pointers`, `run_fill` and `copy_sample_count`
    /// (src/lib/OpenEXR/ImfDeepScanLineInputFile.cpp:784-1058 @ v3.5.3) and of the same in
    /// `TileProcess` (src/lib/OpenEXR/ImfDeepTiledInputFile.cpp:1006-1245 @ v3.5.3), which
    /// differ only in where a chunk starts; the samples' pass checks the chunk's counts
    /// against `fb`'s where C++ copies them over its count slice.
    #[allow(clippy::too_many_arguments)]
    fn deep_chunk<'c>(
        &self,
        deep: &mut Deep<'c>,
        cinfo: &ChunkInfo,
        at: (i32, i32),
        skip: i32,
        ignore: i32,
        counts: &mut [u32],
        window: &Box2i,
        fb: Option<&mut DeepFrameBuffer<'_>>,
    ) -> Result<()>
    where
        'f: 'c,
    {
        let ctx: &'c Context = self.core_context();
        let p = self.index() as i32;
        let counts_only = fb.is_none();
        let flags = match &mut deep.dec {
            None => {
                let mut d = ctx.decoding_initialize(p, cinfo).map_err(core)?;
                d.decode_flags |= EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS;
                d.decode_flags |= EXR_DECODE_SAMPLE_COUNTS_AS_INDIVIDUAL;
                deep.dec = Some(d);
                0
            }
            Some(d) => {
                ctx.decoding_update(p, cinfo, d).map_err(core)?;
                d.decode_flags
            }
        };
        let d = deep.dec.as_mut().expect("initialized");
        if counts_only {
            d.decode_flags |= EXR_DECODE_SAMPLE_DATA_ONLY;
        } else {
            d.decode_flags &= !EXR_DECODE_SAMPLE_DATA_ONLY;
        }
        d.user_line_begin_skip = skip;
        d.user_line_end_ignore = ignore;
        let (cw, rows) = (
            cinfo.width.max(0) as usize,
            (cinfo.height - skip - ignore).max(0) as usize,
        );

        let Some(fb) = fb else {
            if flags != d.decode_flags {
                ctx.decoding_choose_default_routines(p, d).map_err(core)?;
            }
            ctx.decoding_run(p, d, &mut []).map_err(core)?;
            let table = chunk_counts(
                d,
                cinfo.width.max(0) as usize * cinfo.height.max(0) as usize,
            )?;
            for r in 0..rows {
                for x in 0..cw {
                    let i = index(window, at.0 + x as i32, at.1 + r as i32).expect("covered");
                    counts[i] = table[(skip as usize + r) * cw + x] as u32;
                }
            }
            return Ok(());
        };

        // the pointer tables: channel by channel, one pointer a kept pixel, each to the
        // pixel's first sample in its channel's buffer; `outs` holds the buffers, in the
        // slices' order, then the tables
        let names: Vec<Vec<u8>> = fb.slices.keys().cloned().collect();
        let mut tables: Vec<Vec<u8>> = Vec::new();
        for c in d.channels.iter_mut() {
            let k = names.iter().position(|n| *n == c.channel_name);
            let Some(k) = k.filter(|_| c.height != 0) else {
                c.data = None;
                c.user_pixel_stride = 0;
                c.user_line_stride = 0;
                continue;
            };
            let size = fb.slices[&names[k]].pixel_type.size();
            c.user_bytes_per_element = size as i16;
            c.user_data_type = fb.slices[&names[k]].pixel_type.value() as u16;
            c.user_pixel_stride = 8;
            c.user_line_stride = (8 * cw) as i32;
            c.data = Some(BufferRef {
                buffer: names.len() + tables.len(),
                offset: 0,
            });
            let mut table = Vec::with_capacity(rows * cw * 8);
            for r in 0..rows {
                for x in 0..cw {
                    let i = index(window, at.0 + x as i32, at.1 + r as i32).expect("covered");
                    let to = BufferRef {
                        buffer: k,
                        offset: (fb.starts[i] * size as u64) as usize,
                    };
                    table.extend_from_slice(&to.to_pointer().to_le_bytes());
                }
            }
            tables.push(table);
        }
        if flags != d.decode_flags {
            ctx.decoding_choose_default_routines(p, d).map_err(core)?;
        }
        {
            let mut outs: Vec<&mut [u8]> = fb.slices.values_mut().map(|s| &mut *s.data).collect();
            outs.extend(tables.iter_mut().map(|t| t.as_mut_slice()));
            ctx.decoding_run(p, d, &mut outs).map_err(core)?;
        }

        // the chunk's counts must be the frame buffer's, which placed its samples
        let table = chunk_counts(
            d,
            cinfo.width.max(0) as usize * cinfo.height.max(0) as usize,
        )?;
        for r in 0..rows {
            for x in 0..cw {
                let i = index(window, at.0 + x as i32, at.1 + r as i32).expect("covered");
                if table[(skip as usize + r) * cw + x] as u32 != fb.counts[i] {
                    return Err(invalid(format!(
                        "The sample count of pixel ({}, {}) is not the frame buffer's",
                        at.0 + x as i32,
                        at.1 + r as i32
                    )));
                }
            }
        }

        // the channels the part lacks: every sample of each kept pixel
        for name in &names {
            if self.header().channel(name).is_some() {
                continue;
            }
            let s = fb.slices.get_mut(name).expect("a slice");
            let size = s.pixel_type.size();
            let mut bytes = [0u8; 4];
            match s.pixel_type {
                PixelType::Half => bytes[..2].copy_from_slice(
                    &openexr_core::imath_half::imath_float_to_half(s.fill_value as f32)
                        .to_ne_bytes(),
                ),
                PixelType::Float => bytes.copy_from_slice(&(s.fill_value as f32).to_ne_bytes()),
                _ => bytes.copy_from_slice(&(s.fill_value as u32).to_ne_bytes()),
            }
            for r in 0..rows {
                for x in 0..cw {
                    let i = index(window, at.0 + x as i32, at.1 + r as i32).expect("covered");
                    let start = (fb.starts[i] * size as u64) as usize;
                    for k in 0..fb.counts[i] as usize {
                        let o = start + k * size;
                        s.data[o..o + size].copy_from_slice(&bytes[..size]);
                    }
                }
            }
        }
        Ok(())
    }
}

fn check_counts(counts: &[u32], window: &Box2i) -> Result<()> {
    if window.width() == 0 || window.height() == 0 || counts.len() < pixels(window) {
        return Err(invalid(format!(
            "Sample counts hold {} pixels, the window needs {}",
            counts.len(),
            pixels(window)
        )));
    }
    Ok(())
}

/// The pixels `x1..=x2` by `y1..=y2` lie in `window`.
fn covers(window: &Box2i, x1: i32, x2: i32, y1: i32, y2: i32) -> Result<()> {
    if x1 < window.min.x || x2 > window.max.x || y1 < window.min.y || y2 > window.max.y {
        return Err(invalid(format!(
            "Pixels ({x1}, {y1}) - ({x2}, {y2}) are outside the frame buffer's window"
        )));
    }
    Ok(())
}
