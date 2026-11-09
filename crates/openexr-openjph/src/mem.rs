// SPDX-License-Identifier: BSD-2-Clause
// Copyright (c) 2019, Aous Naman

//! Port of `ojph_mem.h` (external/OpenJPH/src/core/openjph/ojph_mem.h:1-290 @ OpenJPH 0.32.0)
//! and `ojph_mem.cpp` (external/OpenJPH/src/core/others/ojph_mem.cpp:1-143 @ OpenJPH 0.32.0):
//! line buffers. Upstream carves every buffer of a codestream out of one allocation
//! (`mem_fixed_allocator`), each aligned to 64 bytes, with `pre_size` elements before it for
//! the wavelet's extension; the port gives each line its own vector of the same layout, a
//! zeroed one where upstream's memory is uninitialized (every sample is written before it is
//! read).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use crate::arch::{BYTE_ALIGNMENT, calc_aligned_size};

/// The bytes of buffers a thread keeps for reuse, at most.
const POOL_BYTES: usize = 256 << 20;

/// A thread's buffers of one element type, by capacity (a power of two), kept for reuse: upstream carves a
/// codestream's buffers out of allocators it keeps from one codestream to the next
/// (`mem_fixed_allocator`, `mem_elastic_allocator`); here each buffer, given back when it is
/// dropped, serves the next chunk the thread decodes or encodes, instead of the allocator
/// being asked again for each line of each chunk (which threads decoding at once contend for).
#[derive(Default)]
pub(crate) struct Pool<T> {
    free: HashMap<usize, Vec<Vec<T>>>,
    bytes: usize,
}

/// An element type a [`Pooled`] buffer holds, with its thread's pool.
pub(crate) trait Poolable: Copy + Default + 'static {
    fn with_pool<R>(f: impl FnOnce(&mut Pool<Self>) -> R) -> Option<R>;
}

macro_rules! poolable {
    ($t:ty, $pool:ident) => {
        thread_local! {
            static $pool: RefCell<Pool<$t>> = RefCell::new(Pool { free: HashMap::new(), bytes: 0 });
        }
        impl Poolable for $t {
            fn with_pool<R>(f: impl FnOnce(&mut Pool<Self>) -> R) -> Option<R> {
                $pool
                    .try_with(|p| p.try_borrow_mut().ok().map(|mut p| f(&mut p)))
                    .ok()
                    .flatten()
            }
        }
    };
}

poolable!(i32, POOL_I32);
poolable!(i64, POOL_I64);
poolable!(u8, POOL_U8);
poolable!(f32, POOL_F32);
poolable!(u32, POOL_U32);
poolable!(u64, POOL_U64);

/// A buffer of `T`s taken zeroed from the thread's [`Pool`] and given back to it when dropped.
#[derive(Default)]
pub(crate) struct Pooled<T: Poolable>(Vec<T>);

impl<T: Poolable> Pooled<T> {
    /// `n` zeros: a buffer of that size class the thread kept, cleared, or a new one.
    pub(crate) fn zeroed(n: usize) -> Self {
        if n == 0 {
            return Pooled(Vec::new());
        }
        let class = n.next_power_of_two();
        let kept = T::with_pool(|p| {
            let v = p.free.get_mut(&class).and_then(Vec::pop);
            if v.is_some() {
                p.bytes -= class * size_of::<T>();
            }
            v
        })
        .flatten();
        let mut v = kept.unwrap_or_else(|| Vec::with_capacity(class));
        v.clear();
        v.resize(n, T::default());
        Pooled(v)
    }
}

impl<T: Poolable> Drop for Pooled<T> {
    fn drop(&mut self) {
        let v = std::mem::take(&mut self.0);
        let class = v.capacity();
        // only buffers this pool made (a power of two of capacity) go back
        if class == 0 || !class.is_power_of_two() {
            return;
        }
        let bytes = class * size_of::<T>();
        T::with_pool(move |p| {
            if p.bytes + bytes <= POOL_BYTES {
                p.bytes += bytes;
                p.free.entry(class).or_default().push(v);
            }
        });
    }
}

impl<T: Poolable> Clone for Pooled<T> {
    fn clone(&self) -> Self {
        let mut c = Pooled::zeroed(self.0.len());
        c.0.copy_from_slice(&self.0);
        c
    }
}

impl<T: Poolable> Deref for Pooled<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T: Poolable> DerefMut for Pooled<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.0
    }
}

impl<T: Poolable + std::fmt::Debug> std::fmt::Debug for Pooled<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

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
    I32(Pooled<i32>),
    I64(Pooled<i64>),
    F32(Pooled<f32>),
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
            data: LineData::I32(Pooled::zeroed(pre_size as usize + reserved(num_ele, 4))),
        }
    }

    /// `line_buf::wrap` on a buffer of `post_alloc_data<si64>`: an integer line of 64 bits.
    pub(crate) fn new_i64(num_ele: usize, pre_size: u32) -> Self {
        LineBuf {
            size: num_ele,
            pre_size,
            flags: LFT_64BIT | LFT_INTEGER,
            data: LineData::I64(Pooled::zeroed(pre_size as usize + reserved(num_ele, 8))),
        }
    }

    /// `line_buf::wrap` on a buffer of `post_alloc_data<float>`.
    pub(crate) fn new_f32(num_ele: usize, pre_size: u32) -> Self {
        LineBuf {
            size: num_ele,
            pre_size,
            flags: LFT_32BIT,
            data: LineData::F32(Pooled::zeroed(pre_size as usize + reserved(num_ele, 4))),
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
