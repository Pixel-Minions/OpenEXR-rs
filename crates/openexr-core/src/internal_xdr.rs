// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `internal_xdr.h` (src/lib/OpenEXRCore/internal_xdr.h:1-306 @ v3.5.3): conversions
//! between the host's byte order and OpenEXR's file order, which is little endian.
//!
//! Upstream chooses the conversion by preprocessor tests of the host's byte order; here
//! `u16::from_le` and friends do the same per target. The in-place array conversions take
//! typed slices: the C versions take `void*` plus an element size and do nothing for sizes
//! other than 2, 4 and 8, which [`XdrElement`] for `u8` reproduces.

// Ported ahead of its callers (attributes, header parsing, coding: Phases 2-4).
#![allow(dead_code)]

/// Port of `one_to_native64` (src/lib/OpenEXRCore/internal_xdr.h:87-91 @ v3.5.3).
#[inline]
pub(crate) fn one_to_native64(v: u64) -> u64 {
    u64::from_le(v)
}

/// Port of `one_from_native64` (src/lib/OpenEXRCore/internal_xdr.h:93-97 @ v3.5.3).
#[inline]
pub(crate) fn one_from_native64(v: u64) -> u64 {
    v.to_le()
}

/// Port of `one_to_native32` (src/lib/OpenEXRCore/internal_xdr.h:127-131 @ v3.5.3).
#[inline]
pub(crate) fn one_to_native32(v: u32) -> u32 {
    u32::from_le(v)
}

/// Port of `one_from_native32` (src/lib/OpenEXRCore/internal_xdr.h:133-137 @ v3.5.3).
#[inline]
pub(crate) fn one_from_native32(v: u32) -> u32 {
    v.to_le()
}

/// Port of `one_to_native_float` (src/lib/OpenEXRCore/internal_xdr.h:165-176 @ v3.5.3).
#[inline]
pub(crate) fn one_to_native_float(v: f32) -> f32 {
    f32::from_bits(one_to_native32(v.to_bits()))
}

/// Port of `one_from_native_float` (src/lib/OpenEXRCore/internal_xdr.h:178-189 @ v3.5.3).
#[inline]
pub(crate) fn one_from_native_float(v: f32) -> f32 {
    f32::from_bits(one_from_native32(v.to_bits()))
}

/// Port of `one_to_native16` (src/lib/OpenEXRCore/internal_xdr.h:193-197 @ v3.5.3).
#[inline]
pub(crate) fn one_to_native16(v: u16) -> u16 {
    u16::from_le(v)
}

/// Port of `one_from_native16` (src/lib/OpenEXRCore/internal_xdr.h:199-203 @ v3.5.3).
#[inline]
pub(crate) fn one_from_native16(v: u16) -> u16 {
    v.to_le()
}

/// The element types of the in-place conversions. `u8` converts to itself, as upstream's
/// `priv_to_native` does for an element size of 1.
pub(crate) trait XdrElement: Copy {
    fn le_to_native(self) -> Self;
    fn native_to_le(self) -> Self;
}

impl XdrElement for u8 {
    fn le_to_native(self) -> Self {
        self
    }
    fn native_to_le(self) -> Self {
        self
    }
}

impl XdrElement for u16 {
    fn le_to_native(self) -> Self {
        one_to_native16(self)
    }
    fn native_to_le(self) -> Self {
        one_from_native16(self)
    }
}

impl XdrElement for u32 {
    fn le_to_native(self) -> Self {
        one_to_native32(self)
    }
    fn native_to_le(self) -> Self {
        one_from_native32(self)
    }
}

impl XdrElement for u64 {
    fn le_to_native(self) -> Self {
        one_to_native64(self)
    }
    fn native_to_le(self) -> Self {
        one_from_native64(self)
    }
}

/// Port of `priv_to_native` (src/lib/OpenEXRCore/internal_xdr.h:233-242 @ v3.5.3), with
/// `priv_to_native16`, `priv_to_native32` and `priv_to_native64` (lines 205-216, 139-150,
/// 99-110): a no-op on little-endian hosts.
pub(crate) fn priv_to_native<T: XdrElement>(vals: &mut [T]) {
    if cfg!(target_endian = "big") {
        for v in vals {
            *v = v.le_to_native();
        }
    }
}

/// Port of `priv_from_native` (src/lib/OpenEXRCore/internal_xdr.h:244-253 @ v3.5.3), with
/// `priv_from_native16`, `priv_from_native32` and `priv_from_native64` (lines 218-229, 152-163,
/// 112-123): a no-op on little-endian hosts.
pub(crate) fn priv_from_native<T: XdrElement>(vals: &mut [T]) {
    if cfg!(target_endian = "big") {
        for v in vals {
            *v = v.native_to_le();
        }
    }
}

/// Port of `unaligned_store16` (src/lib/OpenEXRCore/internal_xdr.h:257-262 @ v3.5.3): writes
/// the first 2 bytes of `dst` in file order.
#[inline]
pub(crate) fn unaligned_store16(dst: &mut [u8], v: u16) {
    dst[..2].copy_from_slice(&v.to_le_bytes());
}

/// Port of `unaligned_store32` (src/lib/OpenEXRCore/internal_xdr.h:264-269 @ v3.5.3): writes
/// the first 4 bytes of `dst` in file order.
#[inline]
pub(crate) fn unaligned_store32(dst: &mut [u8], v: u32) {
    dst[..4].copy_from_slice(&v.to_le_bytes());
}

/// Port of `unaligned_load16` (src/lib/OpenEXRCore/internal_xdr.h:273-279 @ v3.5.3): reads the
/// first 2 bytes of `src`, in file order.
#[inline]
pub(crate) fn unaligned_load16(src: &[u8]) -> u16 {
    u16::from_le_bytes(src[..2].try_into().unwrap())
}

/// Port of `unaligned_load32` (src/lib/OpenEXRCore/internal_xdr.h:281-287 @ v3.5.3): reads the
/// first 4 bytes of `src`, in file order.
#[inline]
pub(crate) fn unaligned_load32(src: &[u8]) -> u32 {
    u32::from_le_bytes(src[..4].try_into().unwrap())
}

/// Port of `unaligned_store32_array` (src/lib/OpenEXRCore/internal_xdr.h:294-304 @ v3.5.3):
/// writes `src` to the start of `dst` in file order.
pub(crate) fn unaligned_store32_array(dst: &mut [u8], src: &[u32]) {
    for (out, v) in dst.as_chunks_mut::<4>().0.iter_mut().zip(src) {
        unaligned_store32(out, *v);
    }
}
