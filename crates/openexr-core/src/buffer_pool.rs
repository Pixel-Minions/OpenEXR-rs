// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Zeroed buffers, reused within a thread (no upstream counterpart: speed only).
//!
//! Upstream allocates its codec and pipeline buffers with `malloc` for each chunk, and the C
//! allocator hands the freed blocks straight back. The port's buffers are zeroed `Vec`s (it
//! cannot read memory it has not written); allocating a large one for each chunk costs fresh
//! pages, which the operating system faults in and clears before the zeroing even starts.
//! Here a buffer given back is kept, and taken again zeroed to the length asked: the same
//! contents as a new one, `n` zeros, so nothing a codec computes can change. A thread keeps
//! at most [`MAX_BUFFERS`] buffers of each element type and [`MAX_BYTES`] bytes in all.

use std::cell::RefCell;
use std::collections::TryReserveError;

/// The most buffers a thread keeps, of each element type.
const MAX_BUFFERS: usize = 16;
/// The most bytes a thread keeps, of each element type.
const MAX_BYTES: usize = 256 << 20;

/// An element type the pool keeps buffers of: zero is its default.
pub(crate) trait Pooled: Copy + Default + 'static {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<Self>>) -> R) -> Option<R>;
}

thread_local! {
    static POOL_U8: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    static POOL_U16: RefCell<Vec<Vec<u16>>> = const { RefCell::new(Vec::new()) };
    static POOL_U32: RefCell<Vec<Vec<u32>>> = const { RefCell::new(Vec::new()) };
    static POOL_I32: RefCell<Vec<Vec<i32>>> = const { RefCell::new(Vec::new()) };
}

impl Pooled for u8 {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<u8>>) -> R) -> Option<R> {
        // (None once the thread's storage is gone, at its exit)
        POOL_U8.try_with(|p| f(&mut p.borrow_mut())).ok()
    }
}

impl Pooled for u16 {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<u16>>) -> R) -> Option<R> {
        POOL_U16.try_with(|p| f(&mut p.borrow_mut())).ok()
    }
}

impl Pooled for u32 {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<u32>>) -> R) -> Option<R> {
        POOL_U32.try_with(|p| f(&mut p.borrow_mut())).ok()
    }
}

impl Pooled for i32 {
    fn with_pool<R>(f: impl FnOnce(&mut Vec<Vec<i32>>) -> R) -> Option<R> {
        POOL_I32.try_with(|p| f(&mut p.borrow_mut())).ok()
    }
}

/// `n` zeros from the thread's pool ([`zeroed`]), given back when dropped: for a codec's
/// working buffers, which upstream `malloc`s (or keeps on the stack) for each chunk. The same
/// contents as `vec![0; n]`, which it falls back to (and aborts as) when no buffer can be had.
pub(crate) struct Kept<T: Pooled>(Vec<T>);

impl<T: Pooled> Kept<T> {
    pub(crate) fn zeroed(n: usize) -> Kept<T> {
        Kept(zeroed(n).unwrap_or_else(|_| vec![T::default(); n]))
    }
}

impl<T: Pooled> Drop for Kept<T> {
    fn drop(&mut self) {
        give_back(std::mem::take(&mut self.0));
    }
}

impl<T: Pooled> std::ops::Deref for Kept<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T: Pooled> std::ops::DerefMut for Kept<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.0
    }
}

impl<T: Pooled + std::fmt::Debug> std::fmt::Debug for Kept<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// `n` zeros: a kept buffer large enough (the smallest), or a new one. A new one that cannot
/// be allocated is an error, not an abort.
pub(crate) fn zeroed<T: Pooled>(n: usize) -> Result<Vec<T>, TryReserveError> {
    let kept = T::with_pool(|p| {
        let best = p
            .iter()
            .enumerate()
            .filter(|(_, v)| v.capacity() >= n)
            .min_by_key(|(_, v)| v.capacity())
            .map(|(i, _)| i);
        best.map(|i| p.swap_remove(i))
    })
    .flatten();
    let mut v = match kept {
        Some(mut v) => {
            v.clear();
            v
        }
        None => {
            let mut v = Vec::new();
            v.try_reserve_exact(n)?;
            v
        }
    };
    v.resize(n, T::default());
    Ok(v)
}

/// `n` bytes whose contents are unspecified: a kept buffer large enough (the smallest), as it
/// was left, or new zeros. For a buffer each user writes before it reads, as upstream's
/// `malloc`ed output buffers are: the compressed buffer of a chunk, whose bound
/// (`exr_compress_max_buffer_size`, with the Huffman coder's spare bytes) is far larger than
/// what a codec writes. Heavy-tier builds fill it with a pattern instead, so that any read of
/// a byte not written first would change an output the checks against Core and the wheel
/// compare.
pub(crate) fn reused(n: usize) -> Result<Vec<u8>, TryReserveError> {
    let kept = u8::with_pool(|p| {
        let best = p
            .iter()
            .enumerate()
            .filter(|(_, v)| v.capacity() >= n)
            .min_by_key(|(_, v)| v.capacity())
            .map(|(i, _)| i);
        best.map(|i| p.swap_remove(i))
    })
    .flatten();
    let mut v = match kept {
        Some(v) => v,
        None => {
            let mut v = Vec::new();
            v.try_reserve_exact(n)?;
            v
        }
    };
    if v.len() >= n {
        v.truncate(n);
    } else {
        v.resize(n, 0);
    }
    if cfg!(feature = "heavy") {
        v.fill(0xA5);
    }
    Ok(v)
}

/// Keeps `v` for a later [`zeroed`] or [`reused`], within the thread's limits; otherwise frees
/// it.
pub(crate) fn give_back<T: Pooled>(v: Vec<T>) {
    let bytes = v.capacity() * size_of::<T>();
    if bytes == 0 || bytes > MAX_BYTES {
        return;
    }
    T::with_pool(move |p| {
        let kept: usize = p.iter().map(|b| b.capacity() * size_of::<T>()).sum();
        if p.len() < MAX_BUFFERS && kept + bytes <= MAX_BYTES {
            p.push(v);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A buffer given back comes again as zeros, of the length asked, whatever it held.
    #[test]
    fn a_kept_buffer_comes_back_zeroed() {
        let mut v = zeroed::<u8>(1000).unwrap();
        v.fill(0xAB);
        give_back(v);
        let w = zeroed::<u8>(600).unwrap();
        assert_eq!(w.len(), 600);
        assert!(w.iter().all(|&b| b == 0));
        let mut x = zeroed::<u16>(10).unwrap();
        x.fill(7);
        give_back(x);
        assert_eq!(zeroed::<u16>(10).unwrap(), vec![0u16; 10]);
    }

    /// A [`Kept`] buffer goes back to the pool when dropped and comes again as zeros.
    #[test]
    fn a_kept_guard_gives_its_buffer_back() {
        let mut k = Kept::<u32>::zeroed(5000);
        k.fill(9);
        let cap = k.capacity();
        drop(k);
        let k = Kept::<u32>::zeroed(4000);
        assert_eq!(k.capacity(), cap);
        assert!(k.iter().all(|&v| v == 0));
    }
}
