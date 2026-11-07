// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `opaque.c` (src/lib/OpenEXRCore/opaque.c:1-316 @ v3.5.3) and of
//! `exr_attr_opaquedata_t` (src/lib/OpenEXRCore/openexr_attr.h:361-396 @ v3.5.3): attributes of
//! types Core does not know, kept packed (the file's bytes) or unpacked (whatever a registered
//! handler made of them).
//!
//! The handler's C function pointers become closures with the same in and out parameters and
//! the same result code. They do not receive the context (a Rust closure captures what it
//! needs). The unpacked value is shared (`Arc`), as C shares the pointer when it copies an
//! unpacked attribute.

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use crate::cfmt::Arg;
use crate::context::{ContextCore, Error};
use crate::errors::{ErrorCode, ExrResult};

/// An unpacked opaque value: upstream's `void* unpacked_data`.
pub type Unpacked = Arc<dyn Any + Send + Sync>;

/// `unpack_func_ptr` (src/lib/OpenEXRCore/openexr_attr.h:371-379 @ v3.5.3):
/// `(data, attrsize, &outsize, &outbuffer) -> result`.
pub type OpaqueUnpackFn =
    Arc<dyn Fn(Option<&[u8]>, i32, &mut i32, &mut Option<Unpacked>) -> ExrResult + Send + Sync>;
/// `pack_func_ptr` (src/lib/OpenEXRCore/openexr_attr.h:380-388 @ v3.5.3):
/// `(data, datasize, &outsize, outbuffer) -> result`, with no buffer when asked for the size.
pub type OpaquePackFn =
    Arc<dyn Fn(Option<&Unpacked>, i32, &mut i32, Option<&mut [u8]>) -> ExrResult + Send + Sync>;
/// `destroy_unpacked_func_ptr` (src/lib/OpenEXRCore/openexr_attr.h:389-391 @ v3.5.3).
pub type OpaqueDestroyFn = Arc<dyn Fn(&Unpacked, i32) + Send + Sync>;

/// Port of `exr_attr_opaquedata_t` (src/lib/OpenEXRCore/openexr_attr.h:361-396 @ v3.5.3).
#[derive(Clone, Default)]
pub struct AttrOpaqueData {
    pub(crate) size: i32,
    pub(crate) unpacked_size: i32,
    pub(crate) packed_alloc_size: i32,
    pub(crate) pad: [u8; 4],
    /// `packed_data`: `None` for NULL.
    pub(crate) packed_data: Option<Vec<u8>>,
    pub(crate) unpacked_data: Option<Unpacked>,
    pub(crate) unpack_func_ptr: Option<OpaqueUnpackFn>,
    pub(crate) pack_func_ptr: Option<OpaquePackFn>,
    pub(crate) destroy_unpacked_func_ptr: Option<OpaqueDestroyFn>,
}

impl fmt::Debug for AttrOpaqueData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttrOpaqueData")
            .field("size", &self.size)
            .field("unpacked_size", &self.unpacked_size)
            .field("packed_alloc_size", &self.packed_alloc_size)
            .field("packed_data", &self.packed_data)
            .field("unpacked_data", &self.unpacked_data.is_some())
            .finish_non_exhaustive()
    }
}

impl AttrOpaqueData {
    /// The `size` field: the packed size.
    pub fn size(&self) -> i32 {
        self.size
    }

    /// The `unpacked_size` field.
    pub fn unpacked_size(&self) -> i32 {
        self.unpacked_size
    }

    /// The `packed_alloc_size` field.
    pub fn packed_alloc_size(&self) -> i32 {
        self.packed_alloc_size
    }

    /// The packed bytes.
    pub fn packed_data(&self) -> Option<&[u8]> {
        self.packed_data.as_deref()
    }

    /// The unpacked value.
    pub fn unpacked_data(&self) -> Option<&Unpacked> {
        self.unpacked_data.as_ref()
    }
}

/// Calls the destroy function on the unpacked value, as upstream does before it lets go of it.
fn destroy_unpacked(u: &AttrOpaqueData) {
    if let (Some(data), Some(destroy)) = (&u.unpacked_data, &u.destroy_unpacked_func_ptr) {
        destroy(data, u.unpacked_size);
    }
}

impl ContextCore {
    /// Port of `exr_attr_opaquedata_init` (src/lib/OpenEXRCore/opaque.c:15-48 @ v3.5.3).
    pub(crate) fn attr_opaquedata_init(
        &self,
        u: &mut AttrOpaqueData,
        b: usize,
    ) -> Result<(), Error> {
        if b > i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid size for opaque data (%llu bytes, must be <= INT32_MAX)",
                &[Arg::U64(b as u64)],
            ));
        }
        *u = AttrOpaqueData::default();
        if b > 0 {
            u.packed_data = Some(vec![0u8; b]);
        }
        u.size = b as i32;
        u.packed_alloc_size = b as i32;
        Ok(())
    }

    /// Port of `exr_attr_opaquedata_create` (src/lib/OpenEXRCore/opaque.c:52-63 @ v3.5.3). Data
    /// shorter than `b` is refused (docs/improvements.md, U-2).
    pub(crate) fn attr_opaquedata_create(
        &self,
        u: &mut AttrOpaqueData,
        b: usize,
        d: Option<&[u8]>,
    ) -> Result<(), Error> {
        self.attr_opaquedata_init(u, b)?;
        if let (Some(d), Some(out)) = (d, u.packed_data.as_mut()) {
            if d.len() < b {
                *u = AttrOpaqueData::default();
                return Err(self.standard_error(ErrorCode::InvalidArgument));
            }
            out.copy_from_slice(&d[..b]);
        }
        Ok(())
    }

    /// Port of `exr_attr_opaquedata_destroy` (src/lib/OpenEXRCore/opaque.c:67-84 @ v3.5.3).
    pub(crate) fn attr_opaquedata_destroy(&self, ud: &mut AttrOpaqueData) {
        destroy_unpacked(ud);
        *ud = AttrOpaqueData::default();
    }

    /// Port of `exr_attr_opaquedata_copy` (src/lib/OpenEXRCore/opaque.c:88-104 @ v3.5.3): the
    /// packed bytes if there are any, else the unpacked value, shared.
    pub(crate) fn attr_opaquedata_copy(
        &self,
        ud: &mut AttrOpaqueData,
        srcud: &AttrOpaqueData,
    ) -> Result<(), Error> {
        if let Some(packed) = &srcud.packed_data {
            return self.attr_opaquedata_create(ud, srcud.size as usize, Some(packed));
        }
        self.attr_opaquedata_init(ud, 0)?;
        self.attr_opaquedata_set_unpacked(ud, srcud.unpacked_data.clone(), srcud.unpacked_size)
    }

    /// Port of `exr_attr_opaquedata_unpack` (src/lib/OpenEXRCore/opaque.c:108-150 @ v3.5.3):
    /// returns `(size, unpacked)`, unpacking through the handler the first time. A failure
    /// of the handler is returned as its code, unreported, as upstream does.
    pub(crate) fn attr_opaquedata_unpack(
        &self,
        u: &mut AttrOpaqueData,
    ) -> Result<(i32, Option<Unpacked>), Error> {
        if u.unpacked_data.is_some() {
            return Ok((u.unpacked_size, u.unpacked_data.clone()));
        }
        let Some(unpack) = u.unpack_func_ptr.clone() else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"No unpack provider specified for opaque data",
            ));
        };
        let mut tmpusz: i32 = 0;
        let mut tmpuptr: Option<Unpacked> = None;
        let rv = unpack(u.packed_data.as_deref(), u.size, &mut tmpusz, &mut tmpuptr);
        if rv != ErrorCode::Success {
            return Err(Error::bare(rv));
        }
        u.unpacked_size = tmpusz;
        u.unpacked_data = tmpuptr.clone();
        Ok((tmpusz, tmpuptr))
    }

    /// Port of `exr_attr_opaquedata_pack` (src/lib/OpenEXRCore/opaque.c:154-229 @ v3.5.3):
    /// returns `(size, packed)`, packing through the handler (asking it for the size first)
    /// when only the unpacked value is held.
    pub(crate) fn attr_opaquedata_pack<'a>(
        &self,
        u: &'a mut AttrOpaqueData,
    ) -> Result<(i32, Option<&'a [u8]>), Error> {
        if u.packed_data.is_some() {
            return Ok((u.size, u.packed_data.as_deref()));
        }
        let Some(pack) = u.pack_func_ptr.clone() else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"No pack provider specified for opaque data",
            ));
        };
        let mut nsize: i32 = 0;
        let rv = pack(u.unpacked_data.as_ref(), u.unpacked_size, &mut nsize, None);
        if rv != ErrorCode::Success {
            return Err(self.print_error(
                rv,
                "Pack function failed finding pack buffer size, unpacked size %d",
                &[u.unpacked_size.into()],
            ));
        }
        if nsize > 0 {
            let mut tmpptr = vec![0u8; nsize as usize];
            u.packed_alloc_size = nsize;
            let rv = pack(
                u.unpacked_data.as_ref(),
                u.unpacked_size,
                &mut nsize,
                Some(&mut tmpptr),
            );
            if rv != ErrorCode::Success {
                nsize = u.packed_alloc_size;
                u.packed_alloc_size = 0;
                return Err(self.print_error(
                    rv,
                    "Pack function failed to pack data, unpacked size %d, packed buffer size %d",
                    &[u.unpacked_size.into(), nsize.into()],
                ));
            }
            u.size = nsize;
            u.packed_data = Some(tmpptr);
            destroy_unpacked(u);
            u.unpacked_data = None;
            u.unpacked_size = 0;
            return Ok((nsize, u.packed_data.as_deref()));
        }
        Ok((0, None))
    }

    /// Port of `exr_attr_opaquedata_set_unpacked` (src/lib/OpenEXRCore/opaque.c:233-266
    /// @ v3.5.3).
    pub(crate) fn attr_opaquedata_set_unpacked(
        &self,
        u: &mut AttrOpaqueData,
        unpacked: Option<Unpacked>,
        sz: i32,
    ) -> Result<(), Error> {
        if sz < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Opaque data given invalid negative size (%d)",
                &[sz.into()],
            ));
        }
        destroy_unpacked(u);
        u.unpacked_data = unpacked;
        u.unpacked_size = sz;
        if u.packed_data.is_some() {
            u.packed_data = None;
            u.size = 0;
            u.packed_alloc_size = 0;
        }
        Ok(())
    }

    /// Port of `exr_attr_opaquedata_set_packed` (src/lib/OpenEXRCore/opaque.c:270-316 @ v3.5.3).
    /// Data shorter than `sz` is refused (U-2).
    pub(crate) fn attr_opaquedata_set_packed(
        &self,
        u: &mut AttrOpaqueData,
        packed: Option<&[u8]>,
        sz: i32,
    ) -> Result<(), Error> {
        if sz < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Opaque data given invalid negative size (%d)",
                &[sz.into()],
            ));
        }
        if packed.is_some_and(|p| p.len() < sz as usize) {
            return Err(self.standard_error(ErrorCode::InvalidArgument));
        }
        // `alloc_fn (0)` returns a valid pointer on both platforms (HeapAlloc, glibc malloc),
        // so a zero-size packed buffer is not NULL.
        let mut nmem = vec![0u8; sz as usize];
        destroy_unpacked(u);
        u.unpacked_data = None;
        u.unpacked_size = 0;
        if let Some(p) = packed {
            nmem.copy_from_slice(&p[..sz as usize]);
        }
        u.packed_data = Some(nmem);
        u.size = sz;
        u.packed_alloc_size = sz;
        Ok(())
    }
}
