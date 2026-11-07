// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `preview.c` (src/lib/OpenEXRCore/preview.c:1-93 @ v3.5.3) and of
//! `exr_attr_preview_t` (src/lib/OpenEXRCore/openexr_attr.h:332-340 @ v3.5.3).

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::errors::ErrorCode;

/// Port of `exr_attr_preview_t` (src/lib/OpenEXRCore/openexr_attr.h:332-340 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrPreview {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) alloc_size: usize,
    /// `rgba`: `None` for NULL.
    pub(crate) rgba: Option<Vec<u8>>,
}

impl AttrPreview {
    /// A preview that does not own its pixels (`alloc_size` 0), as callers build one to pass
    /// to `exr_attr_set_preview`.
    pub fn borrowed(width: u32, height: u32, rgba: Option<&[u8]>) -> AttrPreview {
        AttrPreview {
            width,
            height,
            alloc_size: 0,
            rgba: rgba.map(<[u8]>::to_vec),
        }
    }

    /// The `width` field.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The `height` field.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The `alloc_size` field.
    pub fn alloc_size(&self) -> usize {
        self.alloc_size
    }

    /// The pixels.
    pub fn rgba(&self) -> Option<&[u8]> {
        self.rgba.as_deref()
    }
}

impl Context {
    /// Port of `exr_attr_preview_init` (src/lib/OpenEXRCore/preview.c:14-50 @ v3.5.3).
    pub(crate) fn attr_preview_init(
        &self,
        p: &mut AttrPreview,
        w: u32,
        h: u32,
    ) -> Result<(), Error> {
        let bytes: u64 = u64::from(w) * u64::from(h) * 4;
        if bytes > i32::MAX as u64 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid very large size for preview image (%u x %u - %llu bytes)",
                &[w.into(), h.into(), Arg::U64(bytes)],
            ));
        }
        *p = AttrPreview::default();
        if bytes > 0 {
            p.rgba = Some(vec![0u8; bytes as usize]);
            p.alloc_size = bytes as usize;
            p.width = w;
            p.height = h;
        }
        Ok(())
    }

    /// Port of `exr_attr_preview_create` (src/lib/OpenEXRCore/preview.c:54-76 @ v3.5.3). Pixels
    /// shorter than `w * h * 4` bytes are refused (docs/improvements.md, U-2).
    pub(crate) fn attr_preview_create(
        &self,
        p: &mut AttrPreview,
        w: u32,
        h: u32,
        d: Option<&[u8]>,
    ) -> Result<(), Error> {
        let copybytes = (w as usize) * (h as usize) * 4;
        if copybytes > 0 && d.is_none() {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid NULL preview rgba data for %u x %u preview",
                &[w.into(), h.into()],
            ));
        }
        self.attr_preview_init(p, w, h)?;
        if copybytes > 0
            && let Some(d) = d
        {
            if d.len() < copybytes {
                self.attr_preview_destroy(p);
                return Err(self.core.standard_error(ErrorCode::InvalidArgument));
            }
            p.rgba
                .as_mut()
                .expect("initialised")
                .copy_from_slice(&d[..copybytes]);
        }
        Ok(())
    }

    /// Port of `exr_attr_preview_destroy` (src/lib/OpenEXRCore/preview.c:80-93 @ v3.5.3).
    pub(crate) fn attr_preview_destroy(&self, p: &mut AttrPreview) {
        *p = AttrPreview::default();
    }
}
