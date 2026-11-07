// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `string_vector.c` (src/lib/OpenEXRCore/string_vector.c:1-246 @ v3.5.3) and of
//! `exr_attr_string_vector_t` (src/lib/OpenEXRCore/openexr_attr.h:278-285 @ v3.5.3).
//!
//! `strings` holds `alloc_size` entries, as the C array does; those past `n_strings` are empty.
//! The size limits are upstream's, computed with C's `sizeof (exr_attr_string_t)` on x86-64.

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::context::{Context, Error};
use crate::errors::ErrorCode;
use crate::string::AttrString;

/// `sizeof (exr_attr_string_t)` on the reference platforms: two `int32_t` and a pointer.
const SIZEOF_EXR_ATTR_STRING_T: usize = 16;

/// Port of `exr_attr_string_vector_t` (src/lib/OpenEXRCore/openexr_attr.h:278-285 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrStringVector {
    pub(crate) n_strings: i32,
    pub(crate) alloc_size: i32,
    /// `strings`: empty for NULL, else `alloc_size` entries.
    pub(crate) strings: Vec<AttrString>,
}

impl AttrStringVector {
    /// The `n_strings` field.
    pub fn n_strings(&self) -> i32 {
        self.n_strings
    }

    /// The `alloc_size` field.
    pub fn alloc_size(&self) -> i32 {
        self.alloc_size
    }

    /// Whether `strings` is NULL.
    pub fn is_null(&self) -> bool {
        self.strings.is_empty()
    }

    /// The `n_strings` entries in use.
    pub fn strings(&self) -> &[AttrString] {
        &self.strings[..(self.n_strings.max(0) as usize).min(self.strings.len())]
    }
}

impl Context {
    /// Port of `exr_attr_string_vector_init` (src/lib/OpenEXRCore/string_vector.c:14-56
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_init(
        &self,
        sv: &mut AttrStringVector,
        nent: i32,
    ) -> Result<(), Error> {
        let bytes = (nent as i64 as usize).wrapping_mul(SIZEOF_EXR_ATTR_STRING_T);
        if nent < 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Received request to allocate negative sized string vector (%d entries)",
                &[nent.into()],
            ));
        }
        if bytes > i32::MAX as usize {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid too large size for string vector (%d entries)",
                &[nent.into()],
            ));
        }
        *sv = AttrStringVector::default();
        if bytes > 0 {
            sv.strings = vec![AttrString::default(); nent as usize];
            sv.n_strings = nent;
            sv.alloc_size = nent;
        }
        Ok(())
    }

    /// Port of `exr_attr_string_vector_destroy` (src/lib/OpenEXRCore/string_vector.c:60-80
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_destroy(&self, sv: &mut AttrStringVector) {
        *sv = AttrStringVector::default();
    }

    /// Port of `exr_attr_string_vector_copy` (src/lib/OpenEXRCore/string_vector.c:84-104
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_copy(
        &self,
        sv: &mut AttrStringVector,
        src: &AttrStringVector,
    ) -> Result<(), Error> {
        let mut rv = self.attr_string_vector_init(sv, src.n_strings);
        let mut i = 0;
        while rv.is_ok() && i < src.n_strings as usize {
            let s = &src.strings[i];
            let data = s.str.as_deref();
            rv = self.attr_string_set_with_length(&mut sv.strings[i], data, s.length);
            i += 1;
        }
        if rv.is_err() {
            self.attr_string_vector_destroy(sv);
        }
        rv
    }

    /// Port of `exr_attr_string_vector_init_entry` (src/lib/OpenEXRCore/string_vector.c:108-133
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_init_entry(
        &self,
        sv: &mut AttrStringVector,
        idx: i32,
        len: i32,
    ) -> Result<(), Error> {
        if idx < 0 || idx >= sv.n_strings {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid index (%d of %d) initializing string vector",
                &[idx.into(), sv.n_strings.into()],
            ));
        }
        self.attr_string_init(&mut sv.strings[idx as usize], len)
    }

    /// Port of `exr_attr_string_vector_set_entry_with_length`
    /// (src/lib/OpenEXRCore/string_vector.c:137-165 @ v3.5.3).
    pub(crate) fn attr_string_vector_set_entry_with_length(
        &self,
        sv: &mut AttrStringVector,
        idx: i32,
        s: Option<&[u8]>,
        len: i32,
    ) -> Result<(), Error> {
        if idx < 0 || idx >= sv.n_strings {
            let shown: &[u8] = match s {
                Some(s) => &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())],
                None => b"<nil>",
            };
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid index (%d of %d) assigning string vector ('%s', len %d)",
                &[idx.into(), sv.n_strings.into(), shown.into(), len.into()],
            ));
        }
        self.attr_string_set_with_length(&mut sv.strings[idx as usize], s, len)
    }

    /// Port of `exr_attr_string_vector_set_entry` (src/lib/OpenEXRCore/string_vector.c:169-179
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_set_entry(
        &self,
        sv: &mut AttrStringVector,
        idx: i32,
        s: Option<&[u8]>,
    ) -> Result<(), Error> {
        let len = s.map_or(0, |s| {
            s.iter().position(|&c| c == 0).unwrap_or(s.len()) as i32
        });
        self.attr_string_vector_set_entry_with_length(sv, idx, s, len)
    }

    /// Port of `exr_attr_string_vector_add_entry_with_length`
    /// (src/lib/OpenEXRCore/string_vector.c:183-235 @ v3.5.3): the array grows to twice its
    /// size, or to one past the new count.
    pub(crate) fn attr_string_vector_add_entry_with_length(
        &self,
        sv: &mut AttrStringVector,
        s: Option<&[u8]>,
        len: i32,
    ) -> Result<(), Error> {
        let nent = sv.n_strings + 1;
        if nent > sv.alloc_size {
            let mut allsz = sv.alloc_size.wrapping_mul(2);
            if sv.alloc_size >= i32::MAX / SIZEOF_EXR_ATTR_STRING_T as i32 {
                return Err(self.core.standard_error(ErrorCode::OutOfMemory));
            }
            if nent > allsz {
                allsz = nent + 1;
            }
            sv.strings.resize(allsz as usize, AttrString::default());
            sv.alloc_size = allsz;
        }
        let idx = sv.n_strings as usize;
        self.attr_string_create_with_length(&mut sv.strings[idx], s, len)?;
        sv.n_strings = nent;
        Ok(())
    }

    /// Port of `exr_attr_string_vector_add_entry` (src/lib/OpenEXRCore/string_vector.c:239-246
    /// @ v3.5.3).
    pub(crate) fn attr_string_vector_add_entry(
        &self,
        sv: &mut AttrStringVector,
        s: Option<&[u8]>,
    ) -> Result<(), Error> {
        let len = s.map_or(0, |s| {
            s.iter().position(|&c| c == 0).unwrap_or(s.len()) as i32
        });
        self.attr_string_vector_add_entry_with_length(sv, s, len)
    }
}
