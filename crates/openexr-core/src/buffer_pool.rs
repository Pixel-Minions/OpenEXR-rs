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

/// Keeps `v` for a later [`zeroed`], within the thread's limits; otherwise frees it.
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
}
