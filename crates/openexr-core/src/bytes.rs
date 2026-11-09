// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `bytes.c` (src/lib/OpenEXRCore/bytes.c:1-140 @ v3.5.3) and of `exr_attr_bytes_t`
//! (src/lib/OpenEXRCore/openexr_attr.h:343-351 @ v3.5.3).

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::cfmt::Arg;
use crate::context::{ContextCore, Error};
use crate::errors::ErrorCode;

/// Port of `exr_attr_bytes_t` (src/lib/OpenEXRCore/openexr_attr.h:343-351 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrBytes {
    pub(crate) size: usize,
    /// `data`: `None` for NULL.
    pub(crate) data: Option<Vec<u8>>,
    pub(crate) hint_length: u32,
    /// `type_hint`: `None` for NULL; else `hint_length` bytes and the NUL upstream adds.
    pub(crate) type_hint: Option<Vec<u8>>,
}

impl AttrBytes {
    /// The value a caller fills in to pass to `exr_attr_set_bytes`: `hint_length` and
    /// `type_hint` from `type_hint`, `size` and `data` from `data` (Rust has no struct
    /// literal of the private fields, so this stands in for C's).
    pub fn borrowed(type_hint: &[u8], data: &[u8]) -> AttrBytes {
        AttrBytes {
            size: data.len(),
            data: Some(data.to_vec()),
            hint_length: type_hint.len() as u32,
            type_hint: Some(type_hint.to_vec()),
        }
    }

    /// The `size` field.
    pub fn size(&self) -> usize {
        self.size
    }

    /// The `size` bytes of `data`.
    pub fn data(&self) -> Option<&[u8]> {
        self.data.as_deref()
    }

    /// The `hint_length` field.
    pub fn hint_length(&self) -> u32 {
        self.hint_length
    }

    /// The `type_hint` bytes, its trailing NUL included.
    pub fn type_hint(&self) -> Option<&[u8]> {
        self.type_hint.as_deref()
    }
}

impl ContextCore {
    /// Port of `exr_attr_bytes_init` (src/lib/OpenEXRCore/bytes.c:15-66 @ v3.5.3).
    pub(crate) fn attr_bytes_init(
        &self,
        u: &mut AttrBytes,
        hint_length: u32,
        bytes_length: usize,
    ) -> Result<(), Error> {
        // Note the design choice to disallow lengths > INT32_MAX is borrowed from other files
        // and not rooted in any deep analysis.
        if hint_length as usize > i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid size for type hint (%llu chars, must be <= INT32_MAX)",
                &[Arg::U64(u64::from(hint_length))],
            ));
        }
        if bytes_length > i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid size for bytes data (%llu bytes, must be <= INT32_MAX)",
                &[Arg::U64(bytes_length as u64)],
            ));
        }
        *u = AttrBytes::default();
        // Even though the type hint is supposed to be written and read using the hint length,
        // for people expecting to use C apis that rely on null bytes, we add one now for safety.
        u.type_hint = Some(vec![0u8; hint_length as usize + 1]);
        u.hint_length = hint_length;
        if bytes_length > 0 {
            u.data = Some(vec![0u8; bytes_length]);
        }
        u.size = bytes_length;
        Ok(())
    }

    /// Port of `exr_attr_bytes_create` (src/lib/OpenEXRCore/bytes.c:70-101 @ v3.5.3). A hint or
    /// data slice shorter than its stated length is refused (docs/improvements.md, U-2).
    pub(crate) fn attr_bytes_create(
        &self,
        u: &mut AttrBytes,
        h: u32,
        b: usize,
        t: Option<&[u8]>,
        d: Option<&[u8]>,
    ) -> Result<(), Error> {
        if h > 0 && t.is_none() {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid NULL type hint for bytes attribute with hint length %u",
                &[h.into()],
            ));
        }
        if b > 0 && d.is_none() {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid NULL bytes data for bytes attribute with size %zu",
                &[b.into()],
            ));
        }
        if (h > 0 && t.is_some_and(|t| t.len() < h as usize))
            || (b > 0 && d.is_some_and(|d| d.len() < b))
        {
            return Err(self.standard_error(ErrorCode::InvalidArgument));
        }
        self.attr_bytes_init(u, h, b)?;
        if b > 0
            && let (Some(out), Some(d)) = (u.data.as_mut(), d)
        {
            out.copy_from_slice(&d[..b]);
        }
        if h > 0
            && let (Some(out), Some(t)) = (u.type_hint.as_mut(), t)
        {
            out[..h as usize].copy_from_slice(&t[..h as usize]);
        }
        Ok(())
    }

    /// Port of `exr_attr_bytes_destroy` (src/lib/OpenEXRCore/bytes.c:105-121 @ v3.5.3).
    pub(crate) fn attr_bytes_destroy(&self, ud: &mut AttrBytes) {
        *ud = AttrBytes::default();
    }

    /// Port of `exr_attr_bytes_copy` (src/lib/OpenEXRCore/bytes.c:125-140 @ v3.5.3).
    pub(crate) fn attr_bytes_copy(&self, ud: &mut AttrBytes, src: &AttrBytes) -> Result<(), Error> {
        self.attr_bytes_create(
            ud,
            src.hint_length,
            src.size,
            src.type_hint.as_deref(),
            src.data.as_deref(),
        )
    }
}
