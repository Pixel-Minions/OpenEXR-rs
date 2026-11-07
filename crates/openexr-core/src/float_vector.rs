// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `float_vector.c` (src/lib/OpenEXRCore/float_vector.c:1-143 @ v3.5.3) and of
//! `exr_attr_float_vector_t` (src/lib/OpenEXRCore/openexr_attr.h:288-295 @ v3.5.3).
//!
//! A static vector (`alloc_size` 0) points at the caller's array in C; here it holds a copy.

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::context::{Context, Error};
use crate::errors::ErrorCode;

/// Port of `exr_attr_float_vector_t` (src/lib/OpenEXRCore/openexr_attr.h:288-295 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AttrFloatVector {
    pub(crate) length: i32,
    pub(crate) alloc_size: i32,
    /// `arr`: `None` for NULL.
    pub(crate) arr: Option<Vec<f32>>,
}

impl AttrFloatVector {
    /// The `length` field.
    pub fn length(&self) -> i32 {
        self.length
    }

    /// The `alloc_size` field: 0 when the vector does not own its memory.
    pub fn alloc_size(&self) -> i32 {
        self.alloc_size
    }

    /// Whether `arr` is NULL.
    pub fn is_null(&self) -> bool {
        self.arr.is_none()
    }

    /// The `length` values.
    pub fn arr(&self) -> &[f32] {
        self.arr.as_deref().unwrap_or(&[])
    }
}

impl Context {
    /// Port of `exr_attr_float_vector_init` (src/lib/OpenEXRCore/float_vector.c:14-57 @ v3.5.3):
    /// allocates, but does not fill (the port's memory is zeroed).
    pub(crate) fn attr_float_vector_init(
        &self,
        fv: &mut AttrFloatVector,
        nent: i32,
    ) -> Result<(), Error> {
        let bytes = (nent as i64 as usize).wrapping_mul(std::mem::size_of::<f32>());
        *fv = AttrFloatVector::default();
        if nent < 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Received request to allocate negative sized float vector (%d entries)",
                &[nent.into()],
            ));
        }
        if bytes > i32::MAX as usize {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid too large size for float vector (%d entries)",
                &[nent.into()],
            ));
        } else if bytes > 0 {
            fv.arr = Some(vec![0.0; nent as usize]);
            fv.length = nent;
            fv.alloc_size = nent;
        }
        Ok(())
    }

    /// Port of `exr_attr_float_vector_init_static` (src/lib/OpenEXRCore/float_vector.c:61-94
    /// @ v3.5.3). Upstream reads `nent` values from `arr`; an `arr` shorter than that is refused
    /// (docs/improvements.md, U-2).
    pub(crate) fn attr_float_vector_init_static(
        &self,
        fv: &mut AttrFloatVector,
        arr: Option<&[f32]>,
        nent: i32,
    ) -> Result<(), Error> {
        if nent < 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Received request to allocate negative sized float vector (%d entries)",
                &[nent.into()],
            ));
        }
        let Some(arr) = arr else {
            return Err(self.core.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid reference to float array object to initialize",
            ));
        };
        if arr.len() < nent as usize {
            return Err(self.core.standard_error(ErrorCode::InvalidArgument));
        }
        *fv = AttrFloatVector {
            arr: Some(arr[..nent as usize].to_vec()),
            length: nent,
            alloc_size: 0,
        };
        Ok(())
    }

    /// Port of `exr_attr_float_vector_create` (src/lib/OpenEXRCore/float_vector.c:98-127
    /// @ v3.5.3). An `arr` shorter than `nent` is refused (U-2).
    pub(crate) fn attr_float_vector_create(
        &self,
        fv: &mut AttrFloatVector,
        arr: Option<&[f32]>,
        nent: i32,
    ) -> Result<(), Error> {
        let Some(arr) = arr else {
            return Err(self.core.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid (NULL) arguments to float vector create",
            ));
        };
        self.attr_float_vector_init(fv, nent)?;
        if nent > 0 {
            let bytes = nent as usize * std::mem::size_of::<f32>();
            if bytes <= i32::MAX as usize {
                if arr.len() < nent as usize {
                    self.attr_float_vector_destroy(fv);
                    return Err(self.core.standard_error(ErrorCode::InvalidArgument));
                }
                let outp = fv.arr.as_mut().expect("initialised");
                outp.copy_from_slice(&arr[..nent as usize]);
            }
        }
        Ok(())
    }

    /// Port of `exr_attr_float_vector_destroy` (src/lib/OpenEXRCore/float_vector.c:131-143
    /// @ v3.5.3).
    pub(crate) fn attr_float_vector_destroy(&self, fv: &mut AttrFloatVector) {
        *fv = AttrFloatVector::default();
    }
}
