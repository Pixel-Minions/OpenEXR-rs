// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_mem.h` (external/OpenJPH/src/core/openjph/ojph_mem.h:1-290 @ OpenJPH 0.32.0)
//! and `ojph_mem.cpp` (external/OpenJPH/src/core/others/ojph_mem.cpp:1-143 @ OpenJPH 0.32.0):
//! line buffers. Upstream carves every buffer of a codestream out of one allocation
//! (`mem_fixed_allocator`), each aligned to 64 bytes, with `pre_size` elements before it for
//! the wavelet's extension; the port gives each line its own vector of the same layout, a
//! zeroed one where upstream's memory is uninitialized (every sample is written before it is
//! read).

use crate::arch::{BYTE_ALIGNMENT, calc_aligned_size};

/// `line_buf`'s flags (external/OpenJPH/src/core/openjph/ojph_mem.h:163-171 @ OpenJPH 0.32.0).
pub(crate) const LFT_UNDEFINED: u32 = 0x00;
pub(crate) const LFT_32BIT: u32 = 0x04;
pub(crate) const LFT_64BIT: u32 = 0x08;
pub(crate) const LFT_INTEGER: u32 = 0x10;
pub(crate) const LFT_SIZE_MASK: u32 = 0x0F;

/// A line's samples: `si32`, `si64` or `float` (upstream's union of pointers).
#[derive(Debug, Clone, Default)]
pub(crate) enum LineData {
    #[default]
    None,
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
}

/// Port of `line_buf` (external/OpenJPH/src/core/openjph/ojph_mem.h:160-190 @ OpenJPH 0.32.0):
/// a line of `size` samples with `pre_size` samples before it. `data` holds the samples from
/// the first of the `pre_size` ones: sample `i` of the line is element `pre_size + i`.
#[derive(Debug, Clone, Default)]
pub(crate) struct LineBuf {
    pub(crate) size: usize,
    pub(crate) pre_size: u32,
    pub(crate) flags: u32,
    pub(crate) data: LineData,
}

/// The elements a buffer of `num_ele` samples of `elem` bytes takes after its start, as
/// `post_alloc_data` reserves them (`calc_aligned_size`), with the slack of the alignment.
fn reserved(num_ele: usize, elem: usize) -> usize {
    let n = BYTE_ALIGNMENT as usize;
    calc_aligned_size(num_ele, elem, n) + n.div_ceil(elem)
}

impl LineBuf {
    /// Port of `line_buf::wrap` (external/OpenJPH/src/core/others/ojph_mem.cpp:53-80
    /// @ OpenJPH 0.32.0) on a buffer of `post_alloc_data<si32>`: an integer line of 32 bits.
    pub(crate) fn new_i32(num_ele: usize, pre_size: u32) -> Self {
        LineBuf {
            size: num_ele,
            pre_size,
            flags: LFT_32BIT | LFT_INTEGER,
            data: LineData::I32(vec![0; pre_size as usize + reserved(num_ele, 4)]),
        }
    }

    /// `line_buf::wrap` on a buffer of `post_alloc_data<si64>`: an integer line of 64 bits.
    pub(crate) fn new_i64(num_ele: usize, pre_size: u32) -> Self {
        LineBuf {
            size: num_ele,
            pre_size,
            flags: LFT_64BIT | LFT_INTEGER,
            data: LineData::I64(vec![0; pre_size as usize + reserved(num_ele, 8)]),
        }
    }

    /// `line_buf::wrap` on a buffer of `post_alloc_data<float>`.
    pub(crate) fn new_f32(num_ele: usize, pre_size: u32) -> Self {
        LineBuf {
            size: num_ele,
            pre_size,
            flags: LFT_32BIT,
            data: LineData::F32(vec![0.0; pre_size as usize + reserved(num_ele, 4)]),
        }
    }

    /// The line's `si32` samples from sample 0 (`line->i32`).
    pub(crate) fn i32(&self) -> &[i32] {
        match &self.data {
            LineData::I32(v) => &v[self.pre_size as usize..],
            _ => panic!("a 32-bit integer line"),
        }
    }

    pub(crate) fn i32_mut(&mut self) -> &mut [i32] {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::I32(v) => &mut v[p..],
            _ => panic!("a 32-bit integer line"),
        }
    }

    /// The line's `si64` samples from sample 0 (`line->i64`).
    pub(crate) fn i64(&self) -> &[i64] {
        match &self.data {
            LineData::I64(v) => &v[self.pre_size as usize..],
            _ => panic!("a 64-bit integer line"),
        }
    }

    pub(crate) fn i64_mut(&mut self) -> &mut [i64] {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::I64(v) => &mut v[p..],
            _ => panic!("a 64-bit integer line"),
        }
    }

    /// The line's `float` samples from sample 0 (`line->f32`).
    pub(crate) fn f32(&self) -> &[f32] {
        match &self.data {
            LineData::F32(v) => &v[self.pre_size as usize..],
            _ => panic!("a float line"),
        }
    }

    pub(crate) fn f32_mut(&mut self) -> &mut [f32] {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::F32(v) => &mut v[p..],
            _ => panic!("a float line"),
        }
    }

    /// All of an `si32` line's elements, `pre_size` before sample 0 (`line->i32 - pre_size`).
    pub(crate) fn raw_i32_mut(&mut self) -> (&mut [i32], usize) {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::I32(v) => (v, p),
            _ => panic!("a 32-bit integer line"),
        }
    }

    pub(crate) fn raw_i64_mut(&mut self) -> (&mut [i64], usize) {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::I64(v) => (v, p),
            _ => panic!("a 64-bit integer line"),
        }
    }

    pub(crate) fn raw_f32_mut(&mut self) -> (&mut [f32], usize) {
        let p = self.pre_size as usize;
        match &mut self.data {
            LineData::F32(v) => (v, p),
            _ => panic!("a float line"),
        }
    }
}
