// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `channel_list.c` (src/lib/OpenEXRCore/channel_list.c:1-286 @ v3.5.3) and of
//! `exr_attr_chlist_entry_t` and `exr_attr_chlist_t` (src/lib/OpenEXRCore/openexr_attr.h:310-329
//! @ v3.5.3). The list is kept sorted by name, as `strcmp` orders it.

// Ported ahead of its callers (attributes, header parsing: Phases 2-3).
#![allow(dead_code)]

use crate::cfmt::Arg;
use crate::context::{Context, Error};
use crate::errors::ErrorCode;
use crate::openexr_attr::{PerceptualTreatment, PixelType};
use crate::string::AttrString;

/// Port of `exr_attr_chlist_entry_t` (src/lib/OpenEXRCore/openexr_attr.h:310-320 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrChlistEntry {
    pub name: AttrString,
    pub pixel_type: PixelType,
    pub p_linear: u8,
    pub reserved: [u8; 3],
    pub x_sampling: i32,
    pub y_sampling: i32,
}

/// Port of `exr_attr_chlist_t` (src/lib/OpenEXRCore/openexr_attr.h:323-329 @ v3.5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttrChlist {
    pub(crate) num_channels: i32,
    pub(crate) num_alloced: i32,
    /// `entries`: the `num_channels` entries (empty for NULL).
    pub(crate) entries: Vec<AttrChlistEntry>,
}

impl AttrChlist {
    /// The `num_channels` field.
    pub fn num_channels(&self) -> i32 {
        self.num_channels
    }

    /// The `num_alloced` field.
    pub fn num_alloced(&self) -> i32 {
        self.num_alloced
    }

    /// The channels, sorted by name.
    pub fn entries(&self) -> &[AttrChlistEntry] {
        &self.entries
    }
}

/// The C string `name` points at: its bytes up to the first NUL.
fn c_str(name: &[u8]) -> &[u8] {
    &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())]
}

impl Context {
    /// Port of `exr_attr_chlist_init` (src/lib/OpenEXRCore/channel_list.c:15-50 @ v3.5.3).
    pub(crate) fn attr_chlist_init(
        &self,
        clist: &mut AttrChlist,
        nchans: i32,
    ) -> Result<(), Error> {
        if nchans < 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Negative number of channels requested (%d)",
                &[nchans.into()],
            ));
        }
        *clist = AttrChlist::default();
        clist.entries = Vec::with_capacity(nchans as usize);
        clist.num_alloced = nchans;
        Ok(())
    }

    /// Port of `exr_attr_chlist_add` (src/lib/OpenEXRCore/channel_list.c:54-68 @ v3.5.3).
    pub(crate) fn attr_chlist_add(
        &self,
        clist: &mut AttrChlist,
        name: Option<&[u8]>,
        ptype: PixelType,
        islinear: PerceptualTreatment,
        xsamp: i32,
        ysamp: i32,
    ) -> Result<(), Error> {
        let len = name.map_or(0, |n| c_str(n).len() as i32);
        self.attr_chlist_add_with_length(clist, name, len, ptype, islinear, xsamp, ysamp)
    }

    /// Port of `exr_attr_chlist_add_with_length` (src/lib/OpenEXRCore/channel_list.c:72-220
    /// @ v3.5.3): validates, then inserts in `strcmp` order; the array grows to twice its
    /// size, or to one past the new count.
    pub(crate) fn attr_chlist_add_with_length(
        &self,
        clist: &mut AttrChlist,
        name: Option<&[u8]>,
        namelen: i32,
        ptype: PixelType,
        islinear: PerceptualTreatment,
        xsamp: i32,
        ysamp: i32,
    ) -> Result<(), Error> {
        let maxlen = i32::from(self.max_name_length);
        let cname: &[u8] = name.map(c_str).unwrap_or(b"<NULL>");
        if name.is_none() || cname.is_empty() || namelen == 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Channel name must not be empty, received '%s'",
                &[Arg::Str(cname)],
            ));
        }
        if namelen > maxlen {
            return Err(self.core.print_error(
                ErrorCode::NameTooLong,
                "Channel name must shorter than length allowed by file (%d), received '%s' (%d)",
                &[maxlen.into(), Arg::Str(cname), namelen.into()],
            ));
        }
        if ptype != PixelType::UINT && ptype != PixelType::HALF && ptype != PixelType::FLOAT {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid pixel type specified (%d) adding channel '%s' to list",
                &[ptype.0.into(), Arg::Str(cname)],
            ));
        }
        if islinear != PerceptualTreatment::LOGARITHMIC && islinear != PerceptualTreatment::LINEAR {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid perceptual linear flag value (%d) adding channel '%s' to list",
                &[islinear.0.into(), Arg::Str(cname)],
            ));
        }
        if xsamp <= 0 || ysamp <= 0 {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid pixel sampling (x %d y %d) adding channel '%s' to list",
                &[xsamp.into(), ysamp.into(), Arg::Str(cname)],
            ));
        }

        let mut insertpos = 0usize;
        for (c, entry) in clist.entries.iter().enumerate() {
            match cname.cmp(entry.name.c_str()) {
                std::cmp::Ordering::Less => {
                    insertpos = c;
                    break;
                }
                std::cmp::Ordering::Equal => {
                    return Err(self.core.print_error(
                        ErrorCode::InvalidArgument,
                        "Attempt to add duplicate channel '%s' to channel list",
                        &[Arg::Str(cname)],
                    ));
                }
                std::cmp::Ordering::Greater => insertpos = c + 1,
            }
        }

        let mut nent = AttrChlistEntry::default();
        self.attr_string_create_with_length(&mut nent.name, name, namelen)?;
        let newcount = clist.num_channels + 1;
        nent.pixel_type = ptype;
        nent.p_linear = islinear.0 as u8;
        nent.x_sampling = xsamp;
        nent.y_sampling = ysamp;

        if newcount > clist.num_alloced {
            // perhaps an arbitrary limit, but that is a LOT of channels
            if clist.num_alloced >= i32::MAX / 2 {
                return Err(self.core.standard_error(ErrorCode::OutOfMemory));
            }
            let mut nsz = clist.num_alloced * 2;
            if newcount > nsz {
                nsz = newcount + 1;
            }
            clist
                .entries
                .reserve_exact(nsz as usize - clist.entries.len());
            clist.num_alloced = nsz;
        }
        clist.entries.insert(insertpos, nent);
        clist.num_channels = newcount;
        Ok(())
    }

    /// Port of `exr_attr_chlist_duplicate` (src/lib/OpenEXRCore/channel_list.c:224-264
    /// @ v3.5.3).
    pub(crate) fn attr_chlist_duplicate(
        &self,
        chl: &mut AttrChlist,
        srcchl: &AttrChlist,
    ) -> Result<(), Error> {
        let numchans = srcchl.num_channels;
        if numchans > 0 && srcchl.entries.is_empty() {
            return Err(self.core.print_error(
                ErrorCode::InvalidArgument,
                "Invalid NULL channel list entries with %d channels",
                &[numchans.into()],
            ));
        }
        self.attr_chlist_init(chl, numchans)?;
        for cur in srcchl.entries.iter().take(numchans.max(0) as usize) {
            let rv = self.attr_chlist_add_with_length(
                chl,
                cur.name.str.as_deref(),
                cur.name.length,
                cur.pixel_type,
                PerceptualTreatment(i32::from(cur.p_linear)),
                cur.x_sampling,
                cur.y_sampling,
            );
            if rv.is_err() {
                self.attr_chlist_destroy(chl);
                return rv;
            }
        }
        Ok(())
    }

    /// Port of `exr_attr_chlist_destroy` (src/lib/OpenEXRCore/channel_list.c:268-286 @ v3.5.3).
    pub(crate) fn attr_chlist_destroy(&self, clist: &mut AttrChlist) {
        *clist = AttrChlist::default();
    }
}
