// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `string.c` (src/lib/OpenEXRCore/string.c:1-275 @ v3.5.3) and of `exr_attr_string_t`
//! (src/lib/OpenEXRCore/openexr_attr.h:268-275 @ v3.5.3).
//!
//! The C string keeps its observable state: `length`, `alloc_size` (0 for a "static" string,
//! which upstream points at the caller's memory instead of copying), and the bytes of `str`
//! including its NUL. Here `str` is a buffer the string owns in both cases; for a static
//! string it holds a copy of the caller's bytes. The C argument checks for NULL objects have
//! no counterpart (a reference is never NULL, PLAN.md O8); NULL *data* is `None`.

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::context::{ContextCore, Error};
use crate::errors::ErrorCode;

/// Port of `exr_attr_string_t` (src/lib/OpenEXRCore/openexr_attr.h:268-275 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrString {
    pub(crate) length: i32,
    pub(crate) alloc_size: i32,
    /// `str`: `None` for NULL; otherwise the bytes C can read, a NUL included.
    pub(crate) str: Option<Vec<u8>>,
}

impl AttrString {
    /// The `length` field.
    pub fn length(&self) -> i32 {
        self.length
    }

    /// The `alloc_size` field: 0 when the string does not own its memory.
    pub fn alloc_size(&self) -> i32 {
        self.alloc_size
    }

    /// Whether `str` is NULL.
    pub fn is_null(&self) -> bool {
        self.str.is_none()
    }

    /// The bytes of `str` up to its first NUL, as `strcmp` and `%s` see them.
    pub fn c_str(&self) -> &[u8] {
        match &self.str {
            None => &[],
            Some(b) => {
                let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
                &b[..n]
            }
        }
    }

    /// The `length` bytes at `str`, as upstream writes them to a file.
    pub fn as_bytes(&self) -> &[u8] {
        match &self.str {
            None => &[],
            Some(b) => &b[..(self.length.max(0) as usize).min(b.len())],
        }
    }

    /// The byte at `str[i]`, as C reads it (`None` past the buffer or for NULL).
    pub fn byte(&self, i: usize) -> Option<u8> {
        self.str.as_ref().and_then(|b| b.get(i).copied())
    }
}

/// A Rust string as C sees it: up to its first NUL, as `strlen` and `strcmp` read a `const
/// char*`. The public API cuts its string arguments with this on the way in.
pub(crate) fn cstr(s: &str) -> &str {
    s.split('\0').next().unwrap_or("")
}

/// `strnlen (d, len)` for a byte slice: a slice without a NUL ends where it ends.
fn strnlen(d: &[u8], len: usize) -> usize {
    let n = d.len().min(len);
    d[..n].iter().position(|&c| c == 0).unwrap_or(n)
}

/// `strlen (d)` for a byte slice.
fn strlen(d: &[u8]) -> usize {
    d.iter().position(|&c| c == 0).unwrap_or(d.len())
}

impl ContextCore {
    /// Port of `exr_attr_string_init` (src/lib/OpenEXRCore/string.c:15-40 @ v3.5.3).
    pub(crate) fn attr_string_init(&self, s: &mut AttrString, len: i32) -> Result<(), Error> {
        if len < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Received request to allocate negative sized string (%d)",
                &[len.into()],
            ));
        }
        // `alloc_fn` returns uninitialised memory; the port's is zeroed.
        s.str = Some(vec![0u8; len as usize + 1]);
        s.length = len;
        s.alloc_size = len + 1;
        Ok(())
    }

    /// Port of `exr_attr_string_init_static_with_length` (src/lib/OpenEXRCore/string.c:44-73
    /// @ v3.5.3).
    pub(crate) fn attr_string_init_static_with_length(
        &self,
        s: &mut AttrString,
        v: Option<&[u8]>,
        len: i32,
    ) -> Result<(), Error> {
        if len < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Received request to allocate negative sized string (%d)",
                &[len.into()],
            ));
        }
        let Some(v) = v else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid static string argument to initialize",
            ));
        };
        s.length = len;
        s.alloc_size = 0;
        // Upstream keeps the caller's pointer; the copy holds the same bytes and its NUL.
        let mut copy = v[..strlen(v).max(len as usize).min(v.len())].to_vec();
        copy.push(0);
        s.str = Some(copy);
        Ok(())
    }

    /// Port of `exr_attr_string_init_static` (src/lib/OpenEXRCore/string.c:77-98 @ v3.5.3).
    pub(crate) fn attr_string_init_static(
        &self,
        s: &mut AttrString,
        v: Option<&[u8]>,
    ) -> Result<(), Error> {
        let mut length: i32 = 0;
        if let Some(v) = v {
            let fulllen = strlen(v);
            if fulllen >= i32::MAX as usize {
                return Err(self.report_error(
                    ErrorCode::InvalidArgument,
                    b"Invalid string too long for attribute",
                ));
            }
            length = fulllen as i32;
        }
        self.attr_string_init_static_with_length(s, v, length)
    }

    /// Port of `exr_attr_string_create_with_length` (src/lib/OpenEXRCore/string.c:102-151
    /// @ v3.5.3).
    ///
    /// With NULL data upstream zeroes only `len` of the `len + 1` bytes, leaving the last one
    /// as `alloc_fn` returned it; the port's buffer is zeroed (docs/improvements.md, U-1).
    pub(crate) fn attr_string_create_with_length(
        &self,
        s: &mut AttrString,
        d: Option<&[u8]>,
        len: i32,
    ) -> Result<(), Error> {
        self.attr_string_init(s, len)?;
        let outs = s.str.as_mut().expect("initialised");
        // someone might pass in a string shorter than requested length (or longer)
        if len > 0 {
            if let Some(d) = d {
                let slen = strnlen(d, len as usize);
                outs[..slen].copy_from_slice(&d[..slen]);
                outs[slen..=len as usize].fill(0);
            } else {
                outs[..len as usize].fill(0);
            }
        } else {
            outs[0] = 0;
        }
        Ok(())
    }

    /// Port of `exr_attr_string_create` (src/lib/OpenEXRCore/string.c:155-173 @ v3.5.3).
    pub(crate) fn attr_string_create(
        &self,
        s: &mut AttrString,
        d: Option<&[u8]>,
    ) -> Result<(), Error> {
        let mut len: i32 = 0;
        if let Some(d) = d {
            let fulllen = strlen(d);
            if fulllen >= i32::MAX as usize {
                return Err(self.report_error(
                    ErrorCode::InvalidArgument,
                    b"Invalid string too long for attribute",
                ));
            }
            len = fulllen as i32;
        }
        self.attr_string_create_with_length(s, d, len)
    }

    /// Port of `exr_attr_string_set_with_length` (src/lib/OpenEXRCore/string.c:177-235
    /// @ v3.5.3): reuses the buffer when it is large enough.
    pub(crate) fn attr_string_set_with_length(
        &self,
        s: &mut AttrString,
        d: Option<&[u8]>,
        len: i32,
    ) -> Result<(), Error> {
        if len < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Received request to assign a negative sized string (%d)",
                &[len.into()],
            ));
        }
        if s.alloc_size > len {
            // we own the memory
            s.length = len;
            let sstr = s.str.as_mut().expect("an owned string has a buffer");
            if len > 0 {
                if let Some(d) = d {
                    let slen = strnlen(d, len as usize);
                    sstr[..slen].copy_from_slice(&d[..slen]);
                    sstr[slen..=len as usize].fill(0);
                } else {
                    sstr[..=len as usize].fill(0);
                }
            } else {
                sstr[0] = 0;
            }
            return Ok(());
        }
        self.attr_string_destroy(s);
        self.attr_string_create_with_length(s, d, len)
    }

    /// Port of `exr_attr_string_set` (src/lib/OpenEXRCore/string.c:239-258 @ v3.5.3).
    pub(crate) fn attr_string_set(
        &self,
        s: &mut AttrString,
        d: Option<&[u8]>,
    ) -> Result<(), Error> {
        let mut len: i32 = 0;
        if let Some(d) = d {
            let fulllen = strlen(d);
            if fulllen >= i32::MAX as usize {
                return Err(self.report_error(
                    ErrorCode::InvalidArgument,
                    b"Invalid string too long for attribute",
                ));
            }
            len = fulllen as i32;
        }
        self.attr_string_set_with_length(s, d, len)
    }

    /// Port of `exr_attr_string_destroy` (src/lib/OpenEXRCore/string.c:262-275 @ v3.5.3).
    pub(crate) fn attr_string_destroy(&self, s: &mut AttrString) {
        *s = AttrString::default();
    }
}
