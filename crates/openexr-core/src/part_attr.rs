// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `part_attr.c` (src/lib/OpenEXRCore/part_attr.c:1-2558 @ v3.5.3), so far the read
//! side: the attribute list queries, the getters of the required attributes and the typed
//! `exr_attr_get_*` family. The setters and declarations need write contexts (Phase 6).
//!
//! C's out-pointers become return values; a getter's `NULL output for '%s'` check has no
//! counterpart (PLAN.md O8).

use crate::attributes::{AttrId, AttrValue, Attribute};
use crate::cfmt::Arg;
use crate::channel_list::AttrChlist;
use crate::context::{Context, Error, Part};
use crate::errors::ErrorCode;
use crate::opaque::Unpacked;
use crate::openexr_attr::*;
use crate::openexr_attr::{
    Box2i, Compression, LineOrder, PerceptualTreatment, PixelType, V2f, V2i,
};

/// Port of `exr_attr_list_access_mode_t` (src/lib/OpenEXRCore/openexr_part.h:293-297 @ v3.5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttrListAccessMode {
    /// Order they appear in the file
    FileOrder = 0,
    /// Alphabetically sorted
    SortedOrder = 1,
}

/// What `exr_attr_get_user` returns: the type name, and the unpacked value when the attribute
/// has a pack function, else its packed bytes.
#[derive(Debug)]
pub enum UserAttr<'a> {
    Unpacked {
        type_name: &'a [u8],
        size: i32,
        data: Option<&'a Unpacked>,
    },
    Packed {
        type_name: &'a [u8],
        size: i32,
        data: Option<&'a [u8]>,
    },
}

macro_rules! attr_get {
    ($(#[$doc:meta])* $fn:ident, $t:ident, $variant:ident, $entry:literal, $ret:ty, |$v:ident| $out:expr) => {
        $(#[$doc])*
        pub fn $fn(&self, part_index: i32, name: Option<&str>) -> Result<$ret, Error> {
            let attr = self.attr_find_attr(part_index, name, AttributeType::$t, $entry)?;
            match &attr.value {
                AttrValue::$variant($v) => Ok($out),
                _ => unreachable!("the type was checked"),
            }
        }
    };
}

macro_rules! req_get {
    ($(#[$doc:meta])* $fn:ident, $slot:ident, $cname:literal, $t:ident, $variant:ident, $ret:ty, |$v:ident| $out:expr) => {
        $(#[$doc])*
        pub fn $fn(&self, part_index: i32) -> Result<$ret, Error> {
            let attr = self.req_attr(part_index, |p| p.$slot, $cname, AttributeType::$t)?;
            match &attr.value {
                AttrValue::$variant($v) => Ok($out),
                _ => unreachable!("the type was checked"),
            }
        }
    };
}

impl Context {
    /// `EXR_LOCK_WRITE_AND_DEFINE_PART` (src/lib/OpenEXRCore/internal_structs.h:305-318
    /// @ v3.5.3): part `part_index`, or an out-of-range error.
    pub(crate) fn part_ref(&self, part_index: i32) -> Result<&Part, Error> {
        if part_index < 0 || part_index as usize >= self.parts.len() {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "Part index (%d) out of range",
                &[part_index.into()],
            ));
        }
        Ok(&self.parts[part_index as usize])
    }

    /// `ATTR_FIND_ATTR` (src/lib/OpenEXRCore/part_attr.c:1139-1161 @ v3.5.3): the attribute
    /// `name` of type `t`; `entry` is the union member upstream's messages name.
    fn attr_find_attr(
        &self,
        part_index: i32,
        name: Option<&str>,
        t: AttributeType,
        entry: &str,
    ) -> Result<&Attribute, Error> {
        let part = self.part_ref(part_index)?;
        let name = match name {
            Some(n) if !n.is_empty() => n,
            _ => {
                let msg = format!("Invalid name for {entry} attribute query");
                return Err(self.report_error(ErrorCode::InvalidArgument, msg.as_bytes()));
            }
        };
        let id = self.attr_list_find_by_name(&part.attributes, Some(name.as_bytes()))?;
        let attr = part.attributes.get(id).expect("found");
        if attr.attr_type() != t {
            let fmt = format!("'%s' requested type '{entry}', but stored attributes is type '%s'");
            return Err(self.print_error(
                ErrorCode::AttrTypeMismatch,
                &fmt,
                &[name.into(), Arg::Str(&attr.type_name)],
            ));
        }
        Ok(attr)
    }

    /// The common part of `REQ_ATTR_GET_IMPL` and `REQ_ATTR_GET_IMPL_DEREF`
    /// (src/lib/OpenEXRCore/part_attr.c:585-621 @ v3.5.3): the required attribute in `slot`,
    /// checked to be of type `t`; `EXR_ERR_NO_ATTR_BY_NAME` (unreported) when it is not set.
    fn req_attr(
        &self,
        part_index: i32,
        slot: impl Fn(&Part) -> Option<AttrId>,
        cname: &str,
        t: AttributeType,
    ) -> Result<&Attribute, Error> {
        let part = self.part_ref(part_index)?;
        let Some(attr) = part.attr(slot(part)) else {
            return Err(Error::bare(ErrorCode::NoAttrByName));
        };
        if attr.attr_type() != t {
            return Err(self.print_error(
                ErrorCode::FileBadHeader,
                "Invalid required attribute type '%s' for '%s'",
                &[Arg::Str(&attr.type_name), cname.into()],
            ));
        }
        Ok(attr)
    }

    /// Port of `exr_get_attribute_count` (src/lib/OpenEXRCore/part_attr.c:19-33 @ v3.5.3).
    pub fn get_attribute_count(&self, part_index: i32) -> Result<i32, Error> {
        Ok(self.part_ref(part_index)?.attributes.num_attributes)
    }

    /// Port of `exr_get_attribute_by_index` (src/lib/OpenEXRCore/part_attr.c:36-66 @ v3.5.3).
    pub fn get_attribute_by_index(
        &self,
        part_index: i32,
        mode: AttrListAccessMode,
        idx: i32,
    ) -> Result<&Attribute, Error> {
        let part = self.part_ref(part_index)?;
        if idx < 0 || idx >= part.attributes.num_attributes {
            return Err(self.standard_error(ErrorCode::ArgumentOutOfRange));
        }
        Ok(match mode {
            AttrListAccessMode::SortedOrder => part
                .attributes
                .sorted_entries()
                .nth(idx as usize)
                .expect("in range"),
            AttrListAccessMode::FileOrder => &part.attributes.entries()[idx as usize],
        })
    }

    /// Port of `exr_get_attribute_by_name` (src/lib/OpenEXRCore/part_attr.c:69-91 @ v3.5.3);
    /// a missing attribute is `EXR_ERR_NO_ATTR_BY_NAME`, unreported.
    pub fn get_attribute_by_name(
        &self,
        part_index: i32,
        name: Option<&str>,
    ) -> Result<&Attribute, Error> {
        let part = self.part_ref(part_index)?;
        let id = self.attr_list_find_by_name(&part.attributes, name.map(str::as_bytes))?;
        Ok(part.attributes.get(id).expect("found"))
    }

    /// Port of `exr_get_attribute_list` (src/lib/OpenEXRCore/part_attr.c:94-128 @ v3.5.3): all
    /// of the part's attributes, in `mode` order.
    pub fn get_attribute_list(
        &self,
        part_index: i32,
        mode: AttrListAccessMode,
    ) -> Result<Vec<&Attribute>, Error> {
        let part = self.part_ref(part_index)?;
        Ok(match mode {
            AttrListAccessMode::SortedOrder => part.attributes.sorted_entries().collect(),
            AttrListAccessMode::FileOrder => part.attributes.entries().iter().collect(),
        })
    }

    req_get!(
        /// Port of `exr_get_channels` (src/lib/OpenEXRCore/part_attr.c:649-654 @ v3.5.3).
        get_channels, channels, "channels", CHLIST, Chlist, &AttrChlist, |v| v
    );
    req_get!(
        /// Port of `exr_get_compression` (src/lib/OpenEXRCore/part_attr.c:707-712 @ v3.5.3).
        get_compression, compression, "compression", COMPRESSION, Compression, Compression,
        |v| Compression(i32::from(*v))
    );
    req_get!(
        /// Port of `exr_get_data_window` (src/lib/OpenEXRCore/part_attr.c:733-738 @ v3.5.3).
        get_data_window, data_window_attr, "dataWindow", BOX2I, Box2i, Box2i, |v| *v
    );
    req_get!(
        /// Port of `exr_get_display_window` (src/lib/OpenEXRCore/part_attr.c:769-774 @ v3.5.3).
        get_display_window, display_window_attr, "displayWindow", BOX2I, Box2i, Box2i, |v| *v
    );
    req_get!(
        /// Port of `exr_get_lineorder` (src/lib/OpenEXRCore/part_attr.c:802-807 @ v3.5.3).
        get_lineorder, line_order, "lineOrder", LINEORDER, LineOrder, LineOrder,
        |v| LineOrder(i32::from(*v))
    );
    req_get!(
        /// Port of `exr_get_pixel_aspect_ratio` (src/lib/OpenEXRCore/part_attr.c:837-842
        /// @ v3.5.3).
        get_pixel_aspect_ratio, pixel_aspect_ratio, "pixelAspectRatio", FLOAT, Float, f32, |v| *v
    );
    req_get!(
        /// Port of `exr_get_screen_window_center` (src/lib/OpenEXRCore/part_attr.c:856-861
        /// @ v3.5.3).
        get_screen_window_center, screen_window_center, "screenWindowCenter", V2F, V2f, V2f, |v| *v
    );
    req_get!(
        /// Port of `exr_get_screen_window_width` (src/lib/OpenEXRCore/part_attr.c:884-889
        /// @ v3.5.3).
        get_screen_window_width, screen_window_width, "screenWindowWidth", FLOAT, Float, f32, |v| *v
    );
    req_get!(
        /// Port of `exr_get_version` (src/lib/OpenEXRCore/part_attr.c:1102-1106 @ v3.5.3).
        get_version, version, "version", INT, Int, i32, |v| *v
    );

    /// Port of `exr_get_tile_descriptor` (src/lib/OpenEXRCore/part_attr.c:903-930 @ v3.5.3):
    /// `(x_size, y_size, level mode, round mode)`.
    pub fn get_tile_descriptor(
        &self,
        part_index: i32,
    ) -> Result<(u32, u32, TileLevelMode, TileRoundMode), Error> {
        let attr = self.req_attr(part_index, |p| p.tiles, "tiles", AttributeType::TILEDESC)?;
        let AttrValue::TileDesc(t) = &attr.value else {
            unreachable!("checked")
        };
        Ok((t.x_size, t.y_size, t.level_mode(), t.round_mode()))
    }

    /// Port of `exr_get_name` (src/lib/OpenEXRCore/part_attr.c:999-1019 @ v3.5.3): the bytes of
    /// the part's name.
    pub fn get_name(&self, part_index: i32) -> Result<&[u8], Error> {
        let attr = self.req_attr(part_index, |p| p.name, "name", AttributeType::STRING)?;
        let AttrValue::String(s) = &attr.value else {
            unreachable!("checked")
        };
        Ok(s.c_str())
    }

    attr_get!(
        /// Port of `exr_attr_get_box2i` (src/lib/OpenEXRCore/part_attr.c:1233-1241 @ v3.5.3).
        attr_get_box2i, BOX2I, Box2i, "box2i", Box2i, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_box2f` (src/lib/OpenEXRCore/part_attr.c:1262-1270 @ v3.5.3).
        attr_get_box2f, BOX2F, Box2f, "box2f", Box2f, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_bytes` (src/lib/OpenEXRCore/part_attr.c:1284-1292 @ v3.5.3).
        attr_get_bytes, BYTES, Bytes, "bytes", crate::bytes::AttrBytes, |v| v.clone()
    );
    attr_get!(
        /// Port of `exr_attr_get_channels` (src/lib/OpenEXRCore/part_attr.c:1407-1415 @ v3.5.3).
        attr_get_channels, CHLIST, Chlist, "chlist", &AttrChlist, |v| v
    );
    attr_get!(
        /// Port of `exr_attr_get_chromaticities` (src/lib/OpenEXRCore/part_attr.c:1469-1477
        /// @ v3.5.3).
        attr_get_chromaticities, CHROMATICITIES, Chromaticities, "chromaticities", Chromaticities,
        |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_compression` (src/lib/OpenEXRCore/part_attr.c:1491-1499
        /// @ v3.5.3).
        attr_get_compression, COMPRESSION, Compression, "uc", Compression,
        |v| Compression(i32::from(*v))
    );
    attr_get!(
        /// Port of `exr_attr_get_double` (src/lib/OpenEXRCore/part_attr.c:1529-1534 @ v3.5.3).
        attr_get_double, DOUBLE, Double, "d", f64, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_envmap` (src/lib/OpenEXRCore/part_attr.c:1545-1553 @ v3.5.3).
        attr_get_envmap, ENVMAP, Envmap, "uc", Envmap, |v| Envmap(i32::from(*v))
    );
    attr_get!(
        /// Port of `exr_attr_get_float` (src/lib/OpenEXRCore/part_attr.c:1577-1582 @ v3.5.3).
        attr_get_float, FLOAT, Float, "f", f32, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_float_vector` (src/lib/OpenEXRCore/part_attr.c:1598-1610
        /// @ v3.5.3): `(length, values)`.
        attr_get_float_vector, FLOAT_VECTOR, FloatVector, "floatvector", (i32, &[f32]),
        |v| (v.length, v.arr())
    );
    attr_get!(
        /// Port of `exr_attr_get_int` (src/lib/OpenEXRCore/part_attr.c:1704-1709 @ v3.5.3).
        attr_get_int, INT, Int, "i", i32, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_keycode` (src/lib/OpenEXRCore/part_attr.c:1727-1735 @ v3.5.3).
        attr_get_keycode, KEYCODE, Keycode, "keycode", Keycode, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_lineorder` (src/lib/OpenEXRCore/part_attr.c:1749-1757 @ v3.5.3).
        attr_get_lineorder, LINEORDER, LineOrder, "uc", LineOrder, |v| LineOrder(i32::from(*v))
    );
    attr_get!(
        /// Port of `exr_attr_get_m33f` (src/lib/OpenEXRCore/part_attr.c:1784-1792 @ v3.5.3).
        attr_get_m33f, M33F, M33f, "m33f", M33f, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_m33d` (src/lib/OpenEXRCore/part_attr.c:1806-1814 @ v3.5.3).
        attr_get_m33d, M33D, M33d, "m33d", M33d, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_m44f` (src/lib/OpenEXRCore/part_attr.c:1828-1836 @ v3.5.3).
        attr_get_m44f, M44F, M44f, "m44f", M44f, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_m44d` (src/lib/OpenEXRCore/part_attr.c:1850-1858 @ v3.5.3).
        attr_get_m44d, M44D, M44d, "m44d", M44d, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_preview` (src/lib/OpenEXRCore/part_attr.c:1872-1880 @ v3.5.3).
        attr_get_preview, PREVIEW, Preview, "preview", crate::preview::AttrPreview, |v| v.clone()
    );
    attr_get!(
        /// Port of `exr_attr_get_rational` (src/lib/OpenEXRCore/part_attr.c:1978-1986 @ v3.5.3).
        attr_get_rational, RATIONAL, Rational, "rational", Rational, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_string` (src/lib/OpenEXRCore/part_attr.c:2000-2012 @ v3.5.3):
        /// `(length, bytes up to the NUL)`.
        attr_get_string, STRING, String, "string", (i32, &[u8]), |v| (v.length, v.c_str())
    );
    attr_get!(
        /// Port of `exr_attr_get_tiledesc` (src/lib/OpenEXRCore/part_attr.c:2281-2289 @ v3.5.3).
        attr_get_tiledesc, TILEDESC, TileDesc, "tiledesc", TileDesc, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_timecode` (src/lib/OpenEXRCore/part_attr.c:2317-2325 @ v3.5.3).
        attr_get_timecode, TIMECODE, Timecode, "timecode", Timecode, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v2i` (src/lib/OpenEXRCore/part_attr.c:2339-2347 @ v3.5.3).
        attr_get_v2i, V2I, V2i, "v2i", V2i, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v2f` (src/lib/OpenEXRCore/part_attr.c:2361-2369 @ v3.5.3).
        attr_get_v2f, V2F, V2f, "v2f", V2f, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v2d` (src/lib/OpenEXRCore/part_attr.c:2388-2396 @ v3.5.3).
        attr_get_v2d, V2D, V2d, "v2d", V2d, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v3i` (src/lib/OpenEXRCore/part_attr.c:2410-2418 @ v3.5.3).
        attr_get_v3i, V3I, V3i, "v3i", V3i, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v3f` (src/lib/OpenEXRCore/part_attr.c:2432-2440 @ v3.5.3).
        attr_get_v3f, V3F, V3f, "v3f", V3f, |v| *v
    );
    attr_get!(
        /// Port of `exr_attr_get_v3d` (src/lib/OpenEXRCore/part_attr.c:2454-2462 @ v3.5.3).
        attr_get_v3d, V3D, V3d, "v3d", V3d, |v| *v
    );

    /// Port of `exr_attr_get_string_vector` (src/lib/OpenEXRCore/part_attr.c:2125-2154 @ v3.5.3):
    /// the strings (bytes up to each NUL).
    pub fn attr_get_string_vector(
        &self,
        part_index: i32,
        name: Option<&str>,
    ) -> Result<Vec<&[u8]>, Error> {
        let attr = self.attr_find_attr(
            part_index,
            name,
            AttributeType::STRING_VECTOR,
            "stringvector",
        )?;
        let AttrValue::StringVector(sv) = &attr.value else {
            unreachable!("checked")
        };
        Ok(sv.strings().iter().map(|s| s.c_str()).collect())
    }

    /// Port of `exr_attr_get_user` (src/lib/OpenEXRCore/part_attr.c:2476-2504 @ v3.5.3).
    pub fn attr_get_user(
        &self,
        part_index: i32,
        name: Option<&str>,
    ) -> Result<UserAttr<'_>, Error> {
        let attr = self.attr_find_attr(part_index, name, AttributeType::OPAQUE, "opaque")?;
        let AttrValue::Opaque(o) = &attr.value else {
            unreachable!("checked")
        };
        Ok(if o.pack_func_ptr.is_some() {
            UserAttr::Unpacked {
                type_name: &attr.type_name,
                size: o.unpacked_size,
                data: o.unpacked_data.as_ref(),
            }
        } else {
            UserAttr::Packed {
                type_name: &attr.type_name,
                size: o.packed_alloc_size,
                data: o.packed_data.as_deref(),
            }
        })
    }

    /// Port of `exr_attr_declare_by_type` (src/lib/OpenEXRCore/part_attr.c:130-150 @ v3.5.3).
    pub fn attr_declare_by_type(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        type_: Option<&str>,
    ) -> Result<AttrId, Error> {
        self.part_ref(part_index)?;
        self.require_write()?;
        let Context {
            core,
            parts,
            custom_handlers,
            ..
        } = self;
        let part = &mut parts[part_index as usize];
        core.attr_list_add_by_type(
            custom_handlers,
            &mut part.attributes,
            name.map(str::as_bytes),
            type_.map(str::as_bytes),
            0,
            false,
        )
        .map(|(id, _)| id)
    }

    /// Port of `exr_attr_declare` (src/lib/OpenEXRCore/part_attr.c:152-172 @ v3.5.3).
    pub fn attr_declare(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        type_: AttributeType,
    ) -> Result<AttrId, Error> {
        self.part_ref(part_index)?;
        self.require_write()?;
        let Context {
            core,
            parts,
            custom_handlers,
            ..
        } = self;
        let part = &mut parts[part_index as usize];
        core.attr_list_add(
            custom_handlers,
            &mut part.attributes,
            name.map(str::as_bytes),
            type_,
            0,
            false,
        )
        .map(|(id, _)| id)
    }
}

/// The slot of a required attribute in a part.
pub(crate) type ReqSlot = fn(&mut Part) -> &mut Option<AttrId>;

impl Context {
    /// `REQ_ATTR_FIND_CREATE` (src/lib/OpenEXRCore/part_attr.c:623-645 @ v3.5.3): the required
    /// attribute `cname` of type `t` of a part open for writing, added if absent; returns it.
    fn req_attr_find_create(
        &mut self,
        part_index: i32,
        slot: ReqSlot,
        cname: &str,
        t: AttributeType,
    ) -> Result<AttrId, Error> {
        self.req_attr_find_create_deferred(part_index, slot, cname, t)?
    }

    /// `REQ_ATTR_FIND_CREATE` as the macro leaves it: the mode and type checks return at once
    /// (the outer error), a failure to add the attribute is left in `rv` (the inner result)
    /// for the caller to return when it gets to it.
    pub(crate) fn req_attr_find_create_deferred(
        &mut self,
        part_index: i32,
        slot: ReqSlot,
        cname: &str,
        t: AttributeType,
    ) -> Result<Result<AttrId, Error>, Error> {
        self.part_ref(part_index)?;
        if self.mode == crate::context::ContextMode::Read {
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        if self.mode == crate::context::ContextMode::WritingData {
            return Err(self.standard_error(ErrorCode::AlreadyWroteAttrs));
        }
        let Context {
            core,
            parts,
            custom_handlers,
            ..
        } = self;
        let part = &mut parts[part_index as usize];
        match *slot(part) {
            None => {
                let added = core.attr_list_add(
                    custom_handlers,
                    &mut part.attributes,
                    Some(cname.as_bytes()),
                    t,
                    0,
                    false,
                );
                Ok(added.map(|(id, _)| {
                    *slot(part) = Some(id);
                    id
                }))
            }
            Some(id) => {
                let attr = part.attributes.get(id).expect("slot");
                if attr.attr_type() != t {
                    return Err(core.print_error(
                        ErrorCode::FileBadHeader,
                        "Invalid required attribute type '%s' for '%s'",
                        &[Arg::Str(&attr.type_name), cname.into()],
                    ));
                }
                Ok(Ok(id))
            }
        }
    }

    fn req_value(&mut self, part_index: i32, id: AttrId) -> &mut AttrValue {
        &mut self.parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("attribute")
            .value
    }

    /// Port of `exr_add_channel` (src/lib/OpenEXRCore/part_attr.c:658-675 @ v3.5.3).
    pub fn add_channel(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        ptype: PixelType,
        islinear: PerceptualTreatment,
        xsamp: i32,
        ysamp: i32,
    ) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.channels,
            "channels",
            AttributeType::CHLIST,
        )?;
        let Context { core, parts, .. } = self;
        let AttrValue::Chlist(cl) = &mut parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("attr")
            .value
        else {
            unreachable!("checked")
        };
        core.attr_chlist_add(cl, name.map(str::as_bytes), ptype, islinear, xsamp, ysamp)
    }

    /// Port of `exr_set_channels` (src/lib/OpenEXRCore/part_attr.c:679-703 @ v3.5.3): a copy of
    /// `channels` replaces the part's channel list.
    pub fn set_channels(&mut self, part_index: i32, channels: &AttrChlist) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.channels,
            "channels",
            AttributeType::CHLIST,
        )?;
        let mut clist = AttrChlist::default();
        self.core.attr_chlist_duplicate(&mut clist, channels)?;
        *self.req_value(part_index, id) = AttrValue::Chlist(clist);
        Ok(())
    }

    /// Port of `exr_set_compression` (src/lib/OpenEXRCore/part_attr.c:716-729 @ v3.5.3).
    pub fn set_compression(&mut self, part_index: i32, ctype: Compression) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.compression,
            "compression",
            AttributeType::COMPRESSION,
        )?;
        *self.req_value(part_index, id) = AttrValue::Compression(ctype.0 as u8);
        let part = &mut self.parts[part_index as usize];
        part.comp_type = ctype;
        part.lines_per_chunk = crate::compression::compression_lines_per_chunk(ctype) as i16;
        Ok(())
    }

    /// Port of `exr_set_data_window` (src/lib/OpenEXRCore/part_attr.c:742-765 @ v3.5.3): also
    /// recomputes the tile information.
    pub fn set_data_window(&mut self, part_index: i32, dw: Option<&Box2i>) -> Result<(), Error> {
        let Some(dw) = dw else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Missing value for data window assignment",
            ));
        };
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.data_window_attr,
            "dataWindow",
            AttributeType::BOX2I,
        )?;
        *self.req_value(part_index, id) = AttrValue::Box2i(*dw);
        let Context { core, parts, .. } = self;
        let part = &mut parts[part_index as usize];
        part.data_window = *dw;
        crate::parse_header::internal_exr_compute_tile_information(core, part, true)
    }

    /// Port of `exr_set_display_window` (src/lib/OpenEXRCore/part_attr.c:778-798 @ v3.5.3).
    pub fn set_display_window(&mut self, part_index: i32, dw: Option<&Box2i>) -> Result<(), Error> {
        let Some(dw) = dw else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Missing value for data window assignment",
            ));
        };
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.display_window_attr,
            "displayWindow",
            AttributeType::BOX2I,
        )?;
        *self.req_value(part_index, id) = AttrValue::Box2i(*dw);
        self.parts[part_index as usize].display_window = *dw;
        Ok(())
    }

    /// Port of `exr_set_lineorder` (src/lib/OpenEXRCore/part_attr.c:811-833 @ v3.5.3).
    pub fn set_lineorder(&mut self, part_index: i32, lo: LineOrder) -> Result<(), Error> {
        // `exr_lineorder_t` is `int` under MSVC and `unsigned int` under GCC, so a negative
        // value passes this check on Windows only (docs/improvements.md, I-5)
        let out_of_range = if cfg!(target_env = "msvc") {
            lo.0 >= LineOrder::LAST_TYPE.0
        } else {
            lo.0 as u32 >= LineOrder::LAST_TYPE.0 as u32
        };
        if out_of_range {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "'lineOrder' value for line order (%d) out of range (%d - %d)",
                &[lo.0.into(), 0i32.into(), LineOrder::LAST_TYPE.0.into()],
            ));
        }
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.line_order,
            "lineOrder",
            AttributeType::LINEORDER,
        )?;
        *self.req_value(part_index, id) = AttrValue::LineOrder(lo.0 as u8);
        self.parts[part_index as usize].lineorder = lo;
        Ok(())
    }

    /// Port of `exr_set_pixel_aspect_ratio` (src/lib/OpenEXRCore/part_attr.c:846-852 @ v3.5.3).
    pub fn set_pixel_aspect_ratio(&mut self, part_index: i32, par: f32) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.pixel_aspect_ratio,
            "pixelAspectRatio",
            AttributeType::FLOAT,
        )?;
        *self.req_value(part_index, id) = AttrValue::Float(par);
        Ok(())
    }

    /// Port of `exr_set_screen_window_center` (src/lib/OpenEXRCore/part_attr.c:865-880 @ v3.5.3).
    pub fn set_screen_window_center(
        &mut self,
        part_index: i32,
        swc: Option<&V2f>,
    ) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.screen_window_center,
            "screenWindowCenter",
            AttributeType::V2F,
        )?;
        let Some(swc) = swc else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Missing value for data window assignment",
            ));
        };
        *self.req_value(part_index, id) = AttrValue::V2f(*swc);
        Ok(())
    }

    /// Port of `exr_set_screen_window_width` (src/lib/OpenEXRCore/part_attr.c:893-899 @ v3.5.3).
    pub fn set_screen_window_width(&mut self, part_index: i32, ssw: f32) -> Result<(), Error> {
        let id = self.req_attr_find_create(
            part_index,
            |p| &mut p.screen_window_width,
            "screenWindowWidth",
            AttributeType::FLOAT,
        )?;
        *self.req_value(part_index, id) = AttrValue::Float(ssw);
        Ok(())
    }

    /// Port of `exr_initialize_required_attr` (src/lib/OpenEXRCore/part_attr.c:174-202 @ v3.5.3).
    #[allow(clippy::too_many_arguments)]
    pub fn initialize_required_attr(
        &mut self,
        part_index: i32,
        display_window: Option<&Box2i>,
        data_window: Option<&Box2i>,
        pixelaspectratio: f32,
        screen_window_center: Option<&V2f>,
        screen_window_width: f32,
        lineorder: LineOrder,
        ctype: Compression,
    ) -> Result<(), Error> {
        self.set_compression(part_index, ctype)?;
        self.set_data_window(part_index, data_window)?;
        self.set_display_window(part_index, display_window)?;
        self.set_lineorder(part_index, lineorder)?;
        self.set_pixel_aspect_ratio(part_index, pixelaspectratio)?;
        self.set_screen_window_center(part_index, screen_window_center)?;
        self.set_screen_window_width(part_index, screen_window_width)
    }

    /// Port of `exr_initialize_required_attr_simple` (src/lib/OpenEXRCore/part_attr.c:206-227
    /// @ v3.5.3): both windows `(0, 0) - (width - 1, height - 1)`, aspect ratio 1, centre 0,
    /// width 1, increasing Y.
    pub fn initialize_required_attr_simple(
        &mut self,
        part_index: i32,
        width: i32,
        height: i32,
        ctype: Compression,
    ) -> Result<(), Error> {
        let disp_window = Box2i {
            min: V2i { x: 0, y: 0 },
            max: V2i {
                x: width.wrapping_sub(1),
                y: height.wrapping_sub(1),
            },
        };
        let swc = V2f { x: 0.0, y: 0.0 };
        self.initialize_required_attr(
            part_index,
            Some(&disp_window),
            Some(&disp_window),
            1.0,
            Some(&swc),
            1.0,
            LineOrder::INCREASING_Y,
            ctype,
        )
    }
}
