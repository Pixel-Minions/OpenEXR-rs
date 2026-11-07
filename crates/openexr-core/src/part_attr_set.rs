// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The setter half of `part_attr.c` (src/lib/OpenEXRCore/part_attr.c:1-2558 @ v3.5.3): the
//! typed `exr_attr_set_*` family, `exr_set_tile_descriptor`, `exr_set_name`, `exr_set_version`
//! and `exr_set_chunk_count`. (The getters and the required-attribute setters are in
//! `part_attr.rs`.)
//!
//! The setters create a missing attribute in write and temporary contexts; in a header update
//! (`EXR_CONTEXT_UPDATE_HEADER`) they may only overwrite a value of the same size.

use crate::attributes::{AttrId, AttrValue};
use crate::bytes::AttrBytes;
use crate::cfmt::Arg;
use crate::channel_list::AttrChlist;
use crate::context::{Context, ContextMode, Error};
use crate::errors::ErrorCode;
use crate::opaque::Unpacked;
use crate::openexr_attr::*;
use crate::preview::AttrPreview;

/// A C string argument for a message: `(null)` for NULL, as the C runtime prints it.
fn nm(name: Option<&str>) -> Arg<'_> {
    match name {
        Some(n) => n.into(),
        None => Arg::NullStr,
    }
}

macro_rules! attr_set_value {
    ($(#[$doc:meta])* $fn:ident, $t:ident, $entry:literal, $ty:ty, |$v:ident| $value:expr) => {
        $(#[$doc])*
        pub fn $fn(&mut self, part_index: i32, name: Option<&str>, val: $ty) -> Result<(), Error> {
            let id = self.attr_find_create(part_index, name, AttributeType::$t, $entry)??;
            let $v = val;
            *self.attr_value_mut(part_index, id) = $value;
            Ok(())
        }
    };
}

macro_rules! attr_set_deref {
    ($(#[$doc:meta])* $fn:ident, $t:ident, $entry:literal, $ty:ty, $variant:ident) => {
        $(#[$doc])*
        pub fn $fn(&mut self, part_index: i32, name: Option<&str>, val: Option<&$ty>) -> Result<(), Error> {
            let id = self.attr_find_create(part_index, name, AttributeType::$t, $entry);
            self.set_deref(part_index, name, id, val.map(|v| AttrValue::$variant(*v)), $entry)
        }
    };
}

impl Context {
    /// The two mode checks every setter starts with.
    fn setter_mode_check(&self) -> Result<(), Error> {
        if self.mode == ContextMode::Read {
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        if self.mode == ContextMode::WritingData {
            return Err(self.standard_error(ErrorCode::AlreadyWroteAttrs));
        }
        Ok(())
    }

    /// Whether a missing attribute may be created (write and temporary contexts).
    fn can_create(&self) -> bool {
        self.mode == ContextMode::Write || self.mode == ContextMode::Temporary
    }

    /// `exr_attr_list_find_by_name` on a part: `Ok(None)` for `EXR_ERR_NO_ATTR_BY_NAME`.
    fn find_attr_id(&self, part_index: i32, name: Option<&str>) -> Result<Option<AttrId>, Error> {
        let part = &self.parts[part_index as usize];
        match self.attr_list_find_by_name(&part.attributes, name.map(str::as_bytes)) {
            Ok(id) => Ok(Some(id)),
            Err(e) if e.code == ErrorCode::NoAttrByName => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// `exr_attr_list_add` on a part.
    fn add_attr(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        t: AttributeType,
    ) -> Result<AttrId, Error> {
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
            t,
            0,
            false,
        )
        .map(|(id, _)| id)
    }

    pub(crate) fn attr_value_mut(&mut self, part_index: i32, id: AttrId) -> &mut AttrValue {
        &mut self.parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("attribute of the part")
            .value
    }

    fn attr_type_of(&self, part_index: i32, id: AttrId) -> (AttributeType, Vec<u8>) {
        let a = self.parts[part_index as usize]
            .attributes
            .get(id)
            .expect("attribute");
        (a.attr_type(), a.type_name.clone())
    }

    /// `ATTR_FIND_CREATE` (src/lib/OpenEXRCore/part_attr.c:1179-1212 @ v3.5.3). The macro
    /// returns at once on the mode, lookup and type checks (the outer error) but leaves a
    /// failure to add the attribute in `rv` (the inner result), which `ATTR_SET_IMPL_DEREF`
    /// returns only after its own value check.
    fn attr_find_create(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        t: AttributeType,
        entry: &str,
    ) -> Result<Result<AttrId, Error>, Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        match self.find_attr_id(part_index, name)? {
            None => {
                if !self.can_create() {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                Ok(self.add_attr(part_index, name, t))
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != t {
                    let fmt = format!(
                        "'%s' requested type '{entry}', but stored attributes is type '%s'"
                    );
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        &fmt,
                        &[nm(name), Arg::Str(&tn)],
                    ));
                }
                Ok(Ok(id))
            }
        }
    }

    /// The tail of `ATTR_SET_IMPL_DEREF` (src/lib/OpenEXRCore/part_attr.c:1219-1230 @ v3.5.3):
    /// a missing value is refused after the attribute was found or created, and before a
    /// failure to create it.
    fn set_deref(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        id: Result<Result<AttrId, Error>, Error>,
        val: Option<AttrValue>,
        entry: &str,
    ) -> Result<(), Error> {
        let id = id?;
        let Some(val) = val else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input value for setting '%s', type '%s'",
                &[nm(name), entry.into()],
            ));
        };
        let id = id?;
        *self.attr_value_mut(part_index, id) = val;
        Ok(())
    }

    /// Port of `exr_attr_set_box2i` (src/lib/OpenEXRCore/part_attr.c:1243-1259 @ v3.5.3): the
    /// data and display windows go through their own setters.
    pub fn attr_set_box2i(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&Box2i>,
    ) -> Result<(), Error> {
        if name == Some("dataWindow") {
            return self.set_data_window(part_index, val);
        }
        if name == Some("displayWindow") {
            return self.set_display_window(part_index, val);
        }
        let id = self.attr_find_create(part_index, name, AttributeType::BOX2I, "box2i");
        self.set_deref(
            part_index,
            name,
            id,
            val.map(|v| AttrValue::Box2i(*v)),
            "box2i",
        )
    }

    attr_set_deref!(
        /// Port of `exr_attr_set_box2f` (src/lib/OpenEXRCore/part_attr.c:1272-1280 @ v3.5.3).
        attr_set_box2f, BOX2F, "box2f", Box2f, Box2f
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_chromaticities` (src/lib/OpenEXRCore/part_attr.c:1479-1487
        /// @ v3.5.3).
        attr_set_chromaticities, CHROMATICITIES, "chromaticities", Chromaticities, Chromaticities
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_keycode` (src/lib/OpenEXRCore/part_attr.c:1737-1745 @ v3.5.3).
        attr_set_keycode, KEYCODE, "keycode", Keycode, Keycode
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_m33f` (src/lib/OpenEXRCore/part_attr.c:1794-1802 @ v3.5.3).
        attr_set_m33f, M33F, "m33f", M33f, M33f
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_m33d` (src/lib/OpenEXRCore/part_attr.c:1816-1824 @ v3.5.3).
        attr_set_m33d, M33D, "m33d", M33d, M33d
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_m44f` (src/lib/OpenEXRCore/part_attr.c:1838-1846 @ v3.5.3).
        attr_set_m44f, M44F, "m44f", M44f, M44f
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_m44d` (src/lib/OpenEXRCore/part_attr.c:1860-1868 @ v3.5.3).
        attr_set_m44d, M44D, "m44d", M44d, M44d
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_rational` (src/lib/OpenEXRCore/part_attr.c:1988-1996 @ v3.5.3).
        attr_set_rational, RATIONAL, "rational", Rational, Rational
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_timecode` (src/lib/OpenEXRCore/part_attr.c:2327-2335 @ v3.5.3).
        attr_set_timecode, TIMECODE, "timecode", Timecode, Timecode
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_v2i` (src/lib/OpenEXRCore/part_attr.c:2349-2357 @ v3.5.3).
        attr_set_v2i, V2I, "v2i", V2i, V2i
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_v2d` (src/lib/OpenEXRCore/part_attr.c:2398-2406 @ v3.5.3).
        attr_set_v2d, V2D, "v2d", V2d, V2d
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_v3i` (src/lib/OpenEXRCore/part_attr.c:2420-2428 @ v3.5.3).
        attr_set_v3i, V3I, "v3i", V3i, V3i
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_v3f` (src/lib/OpenEXRCore/part_attr.c:2442-2450 @ v3.5.3).
        attr_set_v3f, V3F, "v3f", V3f, V3f
    );
    attr_set_deref!(
        /// Port of `exr_attr_set_v3d` (src/lib/OpenEXRCore/part_attr.c:2464-2472 @ v3.5.3).
        attr_set_v3d, V3D, "v3d", V3d, V3d
    );

    /// Port of `exr_attr_set_v2f` (src/lib/OpenEXRCore/part_attr.c:2371-2385 @ v3.5.3).
    pub fn attr_set_v2f(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&V2f>,
    ) -> Result<(), Error> {
        if name == Some("screenWindowCenter") {
            return self.set_screen_window_center(part_index, val);
        }
        let id = self.attr_find_create(part_index, name, AttributeType::V2F, "v2f");
        self.set_deref(part_index, name, id, val.map(|v| AttrValue::V2f(*v)), "v2f")
    }

    /// Port of `exr_attr_set_tiledesc` (src/lib/OpenEXRCore/part_attr.c:2291-2314 @ v3.5.3).
    pub fn attr_set_tiledesc(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&TileDesc>,
    ) -> Result<(), Error> {
        if name == Some("tiles") {
            let Some(v) = val else {
                return Err(Error::bare(ErrorCode::InvalidArgument));
            };
            return self.set_tile_descriptor(
                part_index,
                v.x_size,
                v.y_size,
                v.level_mode(),
                v.round_mode(),
            );
        }
        let id = self.attr_find_create(part_index, name, AttributeType::TILEDESC, "tiledesc");
        self.set_deref(
            part_index,
            name,
            id,
            val.map(|v| AttrValue::TileDesc(*v)),
            "tiledesc",
        )
    }

    attr_set_value!(
        /// Port of `exr_attr_set_double` (src/lib/OpenEXRCore/part_attr.c:1536-1541 @ v3.5.3).
        attr_set_double, DOUBLE, "d", f64, |v| AttrValue::Double(v)
    );

    /// The `>= LAST_TYPE` range check of the enum setters: `int` under MSVC, `unsigned int`
    /// under GCC (docs/improvements.md, I-5).
    fn enum_out_of_range(v: i32, last: i32) -> bool {
        if cfg!(target_env = "msvc") {
            v >= last
        } else {
            v as u32 >= last as u32
        }
    }

    /// Port of `exr_attr_set_compression` (src/lib/OpenEXRCore/part_attr.c:1501-1526 @ v3.5.3).
    pub fn attr_set_compression(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        cval: Compression,
    ) -> Result<(), Error> {
        let val = cval.0 as u8;
        if Self::enum_out_of_range(cval.0, Compression::LAST_TYPE.0) {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "'%s' value for compression type (%d) out of range (%d - %d)",
                &[
                    nm(name),
                    cval.0.into(),
                    0i32.into(),
                    Compression::LAST_TYPE.0.into(),
                ],
            ));
        }
        if name == Some("compression") {
            return self.set_compression(part_index, cval);
        }
        let id = self.attr_find_create(part_index, name, AttributeType::COMPRESSION, "uc")??;
        *self.attr_value_mut(part_index, id) = AttrValue::Compression(val);
        Ok(())
    }

    /// Port of `exr_attr_set_envmap` (src/lib/OpenEXRCore/part_attr.c:1555-1574 @ v3.5.3).
    pub fn attr_set_envmap(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        eval: Envmap,
    ) -> Result<(), Error> {
        let val = eval.0 as u8;
        if Self::enum_out_of_range(eval.0, Envmap::LAST_TYPE.0) {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "'%s' value for envmap (%d) out of range (%d - %d)",
                &[
                    nm(name),
                    eval.0.into(),
                    0i32.into(),
                    Envmap::LAST_TYPE.0.into(),
                ],
            ));
        }
        let id = self.attr_find_create(part_index, name, AttributeType::ENVMAP, "uc")??;
        *self.attr_value_mut(part_index, id) = AttrValue::Envmap(val);
        Ok(())
    }

    /// Port of `exr_attr_set_float` (src/lib/OpenEXRCore/part_attr.c:1584-1597 @ v3.5.3).
    pub fn attr_set_float(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: f32,
    ) -> Result<(), Error> {
        if name == Some("pixelAspectRatio") {
            return self.set_pixel_aspect_ratio(part_index, val);
        }
        if name == Some("screenWindowWidth") {
            return self.set_screen_window_width(part_index, val);
        }
        let id = self.attr_find_create(part_index, name, AttributeType::FLOAT, "f")??;
        *self.attr_value_mut(part_index, id) = AttrValue::Float(val);
        Ok(())
    }

    /// Port of `exr_attr_set_int` (src/lib/OpenEXRCore/part_attr.c:1711-1724 @ v3.5.3).
    pub fn attr_set_int(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: i32,
    ) -> Result<(), Error> {
        if name == Some("version") {
            return self.set_version(part_index, val);
        }
        if name == Some("chunkCount") {
            return self.set_chunk_count(part_index, val);
        }
        let id = self.attr_find_create(part_index, name, AttributeType::INT, "i")??;
        *self.attr_value_mut(part_index, id) = AttrValue::Int(val);
        Ok(())
    }

    /// Port of `exr_attr_set_lineorder` (src/lib/OpenEXRCore/part_attr.c:1759-1781 @ v3.5.3).
    pub fn attr_set_lineorder(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        lval: LineOrder,
    ) -> Result<(), Error> {
        let val = lval.0 as u8;
        if Self::enum_out_of_range(lval.0, LineOrder::LAST_TYPE.0) {
            return Err(self.print_error(
                ErrorCode::ArgumentOutOfRange,
                "'%s' value for line order enum (%d) out of range (%d - %d)",
                &[
                    nm(name),
                    lval.0.into(),
                    0i32.into(),
                    LineOrder::LAST_TYPE.0.into(),
                ],
            ));
        }
        if name == Some("lineOrder") {
            return self.set_lineorder(part_index, LineOrder(i32::from(val)));
        }
        let id = self.attr_find_create(part_index, name, AttributeType::LINEORDER, "uc")??;
        *self.attr_value_mut(part_index, id) = AttrValue::LineOrder(val);
        Ok(())
    }

    /// Port of `exr_set_tile_descriptor` (src/lib/OpenEXRCore/part_attr.c:936-997 @ v3.5.3).
    pub fn set_tile_descriptor(
        &mut self,
        part_index: i32,
        x_size: u32,
        y_size: u32,
        level_mode: TileLevelMode,
        round_mode: TileRoundMode,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        let storage = self.parts[part_index as usize].storage_mode;
        if storage == Storage::SCANLINE || storage == Storage::DEEP_SCANLINE {
            return Err(self.report_error(
                ErrorCode::TileScanMixedapi,
                b"Attempt to set tile descriptor on scanline part",
            ));
        }
        let id = match self.parts[part_index as usize].tiles {
            None => {
                let id = self.add_attr(part_index, Some("tiles"), AttributeType::TILEDESC)?;
                self.parts[part_index as usize].tiles = Some(id);
                id
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::TILEDESC {
                    return Err(self.print_error(
                        ErrorCode::FileBadHeader,
                        "Invalid required attribute type '%s' for '%s'",
                        &[Arg::Str(&tn), "tiles".into()],
                    ));
                }
                id
            }
        };
        *self.attr_value_mut(part_index, id) = AttrValue::TileDesc(TileDesc {
            x_size,
            y_size,
            level_and_round: pack_tile_level_round(level_mode, round_mode),
        });
        let Context { core, parts, .. } = self;
        crate::parse_header::internal_exr_compute_tile_information(
            core,
            &mut parts[part_index as usize],
            true,
        )
    }

    /// Port of `exr_set_name` (src/lib/OpenEXRCore/part_attr.c:1021-1100 @ v3.5.3). A failure
    /// to add the attribute is returned after the value checks, as upstream returns it.
    pub fn set_name(&mut self, part_index: i32, val: Option<&str>) -> Result<(), Error> {
        let rv = self.req_attr_find_create_deferred(
            part_index,
            |p| &mut p.name,
            "name",
            AttributeType::STRING,
        )?;
        // old library allowed an empty string :(, but ensure not null
        let Some(val) = val else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"Invalid string passed trying to set 'name'",
            ));
        };
        let bytes = val.len();
        if bytes >= i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "String too large to store (%llu bytes) into 'name'",
                &[Arg::U64(bytes as u64)],
            ));
        }
        let id = rv?;
        if self.parts.len() > 1 {
            for (pidx, p) in self.parts.iter().enumerate() {
                if pidx as i32 == part_index {
                    continue;
                }
                let Some(AttrValue::String(pname)) = p.attr(p.name).map(|a| &a.value) else {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Part %d missing required attribute 'name' for multi-part file",
                        &[(pidx as i32).into()],
                    ));
                };
                if val.as_bytes() == pname.c_str() {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Each part should have a unique name, part %d and %d attempting to have same name '%s'",
                        &[(pidx as i32).into(), part_index.into(), val.into()],
                    ));
                }
            }
        }
        let can_create = self.can_create();
        let Context { core, parts, .. } = self;
        let AttrValue::String(s) = &mut parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("name")
            .value
        else {
            unreachable!("checked")
        };
        if s.length == bytes as i32 && s.alloc_size > 0 {
            // we own the string...
            s.str.as_mut().expect("owned")[..bytes].copy_from_slice(val.as_bytes());
            Ok(())
        } else if !can_create {
            let len = s.length;
            Err(core.print_error(
                ErrorCode::ModifySizeChange,
                "Existing string 'name' has length %d, requested %d, unable to change",
                &[len.into(), (bytes as i32).into()],
            ))
        } else {
            core.attr_string_set_with_length(s, Some(val.as_bytes()), bytes as i32)
        }
    }

    /// Port of `exr_set_version` (src/lib/OpenEXRCore/part_attr.c:1110-1122 @ v3.5.3): only 1
    /// is valid (deep data), refused unreported otherwise.
    pub fn set_version(&mut self, part_index: i32, val: i32) -> Result<(), Error> {
        if val <= 0 || val > 1 {
            return Err(Error::bare(ErrorCode::ArgumentOutOfRange));
        }
        let id = self.req_attr_find_create_deferred(
            part_index,
            |p| &mut p.version,
            "version",
            AttributeType::INT,
        )??;
        *self.attr_value_mut(part_index, id) = AttrValue::Int(val);
        Ok(())
    }

    /// Port of `exr_set_chunk_count` (src/lib/OpenEXRCore/part_attr.c:1125-1136 @ v3.5.3).
    pub fn set_chunk_count(&mut self, part_index: i32, val: i32) -> Result<(), Error> {
        let id = self.req_attr_find_create_deferred(
            part_index,
            |p| &mut p.chunk_count_attr,
            "chunkCount",
            AttributeType::INT,
        )??;
        *self.attr_value_mut(part_index, id) = AttrValue::Int(val);
        self.parts[part_index as usize].chunk_count = val;
        Ok(())
    }

    /// Port of `exr_attr_set_bytes` (src/lib/OpenEXRCore/part_attr.c:1294-1405 @ v3.5.3).
    pub fn attr_set_bytes(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&AttrBytes>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        let Some(val) = val else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input value for setting '%s', type 'bytes'",
                &[nm(name)],
            ));
        };
        let hint = val.type_hint.as_deref();
        let data = val.data.as_deref();
        match self.find_attr_id(part_index, name)? {
            None => {
                if !self.can_create() {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let id = self.add_attr(part_index, name, AttributeType::BYTES)?;
                let mut b = AttrBytes::default();
                let rv = self
                    .core
                    .attr_bytes_create(&mut b, val.hint_length, val.size, hint, data);
                *self.attr_value_mut(part_index, id) = AttrValue::Bytes(b);
                rv
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::BYTES {
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        "'%s' requested type 'bytes', but attribute is type '%s'",
                        &[nm(name), Arg::Str(&tn)],
                    ));
                }
                let can_create = self.can_create();
                let Context { core, parts, .. } = self;
                let AttrValue::Bytes(cur) = &mut parts[part_index as usize]
                    .attributes
                    .get_mut(id)
                    .expect("attr")
                    .value
                else {
                    unreachable!("checked")
                };
                if cur.size == val.size && cur.hint_length == val.hint_length {
                    if val.hint_length > 0 && hint.is_none() {
                        return Err(core.print_error(
                            ErrorCode::InvalidArgument,
                            "Invalid NULL type hint for setting '%s'",
                            &[nm(name)],
                        ));
                    }
                    if val.size > 0 && data.is_none() {
                        return Err(core.print_error(
                            ErrorCode::InvalidArgument,
                            "Invalid NULL bytes data for setting '%s'",
                            &[nm(name)],
                        ));
                    }
                    let h = val.hint_length as usize;
                    // a slice shorter than its stated length (docs/improvements.md, U-2)
                    if hint.is_some_and(|s| s.len() < h) || data.is_some_and(|s| s.len() < val.size)
                    {
                        return Err(core.standard_error(ErrorCode::InvalidArgument));
                    }
                    if h > 0
                        && let (Some(dst), Some(src)) = (cur.type_hint.as_mut(), hint)
                    {
                        dst[..h].copy_from_slice(&src[..h]);
                    }
                    if val.size > 0
                        && let (Some(dst), Some(src)) = (cur.data.as_mut(), data)
                    {
                        dst[..val.size].copy_from_slice(&src[..val.size]);
                    }
                    Ok(())
                } else if !can_create {
                    let cs = cur.size;
                    Err(core.print_error(
                        ErrorCode::ModifySizeChange,
                        "Existing bytes '%s' is %zu, requested is %zu, unable to change",
                        &[nm(name), Arg::Usize(cs), Arg::Usize(val.size)],
                    ))
                } else {
                    core.attr_bytes_destroy(cur);
                    core.attr_bytes_create(cur, val.hint_length, val.size, hint, data)
                }
            }
        }
    }

    /// Port of `exr_attr_set_channels` (src/lib/OpenEXRCore/part_attr.c:1417-1467 @ v3.5.3). An
    /// existing attribute of another type is refused (docs/improvements.md, U-4).
    pub fn attr_set_channels(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        channels: Option<&AttrChlist>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        if name == Some("channels") {
            let Some(c) = channels else {
                return Err(self.report_error(
                    ErrorCode::InvalidArgument,
                    b"No channels provided for channel list",
                ));
            };
            return self.set_channels(part_index, c);
        }
        // do not support updating channels during update operation...
        if !self.can_create() {
            return Err(self.standard_error(ErrorCode::NotOpenWrite));
        }
        let Some(channels) = channels else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input values for setting '%s', type 'chlist'",
                &[nm(name)],
            ));
        };
        let id = match self.find_attr_id(part_index, name)? {
            None => self.add_attr(part_index, name, AttributeType::CHLIST)?,
            Some(id) => {
                if self.attr_type_of(part_index, id).0 != AttributeType::CHLIST {
                    return Err(self.standard_error(ErrorCode::AttrTypeMismatch));
                }
                id
            }
        };
        let mut clist = AttrChlist::default();
        self.core.attr_chlist_duplicate(&mut clist, channels)?;
        *self.attr_value_mut(part_index, id) = AttrValue::Chlist(clist);
        Ok(())
    }

    /// Port of `exr_attr_set_float_vector` (src/lib/OpenEXRCore/part_attr.c:1612-1702 @ v3.5.3).
    pub fn attr_set_float_vector(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        sz: i32,
        val: Option<&[f32]>,
    ) -> Result<(), Error> {
        let bytes = (sz as i64 as usize).wrapping_mul(4);
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        if sz < 0 || bytes > i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid size (%d) for float vector '%s'",
                &[sz.into(), nm(name)],
            ));
        }
        let Some(val) = val else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input values for setting '%s', type 'floatvector'",
                &[nm(name)],
            ));
        };
        match self.find_attr_id(part_index, name)? {
            None => {
                if !self.can_create() {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let id = self.add_attr(part_index, name, AttributeType::FLOAT_VECTOR)?;
                let mut fv = crate::float_vector::AttrFloatVector::default();
                let rv = self.core.attr_float_vector_create(&mut fv, Some(val), sz);
                *self.attr_value_mut(part_index, id) = AttrValue::FloatVector(fv);
                rv
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::FLOAT_VECTOR {
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        "'%s' requested type 'floatvector', but attribute is type '%s'",
                        &[nm(name), Arg::Str(&tn)],
                    ));
                }
                let can_create = self.can_create();
                let Context { core, parts, .. } = self;
                let AttrValue::FloatVector(fv) = &mut parts[part_index as usize]
                    .attributes
                    .get_mut(id)
                    .expect("attr")
                    .value
                else {
                    unreachable!("checked")
                };
                if fv.length == sz && fv.alloc_size > 0 {
                    if val.len() < sz as usize {
                        return Err(core.standard_error(ErrorCode::InvalidArgument));
                    }
                    fv.arr
                        .as_mut()
                        .expect("owned")
                        .copy_from_slice(&val[..sz as usize]);
                    Ok(())
                } else if !can_create {
                    let len = fv.length;
                    Err(core.print_error(
                        ErrorCode::ModifySizeChange,
                        "Existing float vector '%s' has %d, requested %d, unable to change",
                        &[nm(name), len.into(), sz.into()],
                    ))
                } else {
                    core.attr_float_vector_destroy(fv);
                    core.attr_float_vector_create(fv, Some(val), sz)
                }
            }
        }
    }

    /// Port of `exr_attr_set_preview` (src/lib/OpenEXRCore/part_attr.c:1882-1976 @ v3.5.3).
    pub fn attr_set_preview(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&AttrPreview>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        let found = self.find_attr_id(part_index, name);
        let Some(val) = val else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input value for setting '%s', type 'preview'",
                &[nm(name)],
            ));
        };
        let rgba = val.rgba.as_deref();
        match found? {
            None => {
                if !self.can_create() {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let id = self.add_attr(part_index, name, AttributeType::PREVIEW)?;
                let mut p = AttrPreview::default();
                let rv = self
                    .core
                    .attr_preview_create(&mut p, val.width, val.height, rgba);
                *self.attr_value_mut(part_index, id) = AttrValue::Preview(p);
                rv
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::PREVIEW {
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        "'%s' requested type 'preview', but attribute is type '%s'",
                        &[nm(name), Arg::Str(&tn)],
                    ));
                }
                let can_create = self.can_create();
                let Context { core, parts, .. } = self;
                let AttrValue::Preview(cur) = &mut parts[part_index as usize]
                    .attributes
                    .get_mut(id)
                    .expect("attr")
                    .value
                else {
                    unreachable!("checked")
                };
                if cur.width == val.width && cur.height == val.height && cur.alloc_size > 0 {
                    let copybytes = val.width as usize * val.height as usize * 4;
                    if copybytes > 0 && rgba.is_none() {
                        return Err(core.print_error(
                            ErrorCode::InvalidArgument,
                            "Invalid NULL preview rgba data for setting '%s'",
                            &[nm(name)],
                        ));
                    }
                    if let (Some(dst), Some(src)) = (cur.rgba.as_mut(), rgba) {
                        if src.len() < copybytes {
                            return Err(core.standard_error(ErrorCode::InvalidArgument));
                        }
                        dst[..copybytes].copy_from_slice(&src[..copybytes]);
                    }
                    Ok(())
                } else if !can_create {
                    let (w, h) = (cur.width, cur.height);
                    Err(core.print_error(
                        ErrorCode::ModifySizeChange,
                        "Existing preview '%s' is %u x %u, requested is %u x %u, unable to change",
                        &[
                            nm(name),
                            w.into(),
                            h.into(),
                            val.width.into(),
                            val.height.into(),
                        ],
                    ))
                } else {
                    core.attr_preview_destroy(cur);
                    core.attr_preview_create(cur, val.width, val.height, rgba)
                }
            }
        }
    }

    /// Port of `exr_attr_set_string` (src/lib/OpenEXRCore/part_attr.c:2014-2124 @ v3.5.3): `name`
    /// goes through `exr_set_name`; `type` may only be set in a temporary context, where it
    /// also sets the part's storage.
    pub fn attr_set_string(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        val: Option<&str>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        if name == Some("name") {
            return self.set_name(part_index, val);
        }
        if name == Some("type") {
            if self.mode == ContextMode::Temporary {
                let Some(v) = val else {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Part type attribute must be set to valid value",
                        &[],
                    ));
                };
                self.parts[part_index as usize].storage_mode = match v {
                    "scanlineimage" => Storage::SCANLINE,
                    "tiledimage" => Storage::TILED,
                    "deepscanline" => Storage::DEEP_SCANLINE,
                    "deeptile" => Storage::DEEP_TILED,
                    _ => Storage::UNKNOWN,
                };
            } else {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Part type attribute must be implicitly only when adding a part",
                    &[],
                ));
            }
        }
        self.setter_mode_check()?;
        let found = self.find_attr_id(part_index, name);
        let bytes = val.map_or(0, str::len);
        if bytes > i32::MAX as usize {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "String too large to store (%llu bytes) into '%s'",
                &[Arg::U64(bytes as u64), nm(name)],
            ));
        }
        let vb = val.map(str::as_bytes);
        match found? {
            None => {
                if !self.can_create() {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let id = self.add_attr(part_index, name, AttributeType::STRING)?;
                let mut s = crate::string::AttrString::default();
                let rv = self
                    .core
                    .attr_string_create_with_length(&mut s, vb, bytes as i32);
                *self.attr_value_mut(part_index, id) = AttrValue::String(s);
                rv
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::STRING {
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        "'%s' requested type 'string', but attribute is type '%s'",
                        &[nm(name), Arg::Str(&tn)],
                    ));
                }
                let can_create = self.can_create();
                let Context { core, parts, .. } = self;
                let AttrValue::String(s) = &mut parts[part_index as usize]
                    .attributes
                    .get_mut(id)
                    .expect("attr")
                    .value
                else {
                    unreachable!("checked")
                };
                if s.length == bytes as i32 && s.alloc_size > 0 {
                    if let Some(v) = vb {
                        s.str.as_mut().expect("owned")[..bytes].copy_from_slice(v);
                    }
                    Ok(())
                } else if !can_create {
                    let len = s.length;
                    Err(core.print_error(
                        ErrorCode::ModifySizeChange,
                        "Existing string '%s' has length %d, requested %d, unable to change",
                        &[nm(name), len.into(), (bytes as i32).into()],
                    ))
                } else {
                    core.attr_string_set_with_length(s, vb, bytes as i32)
                }
            }
        }
    }

    /// Port of `exr_attr_set_string_vector` (src/lib/OpenEXRCore/part_attr.c:2156-2279 @ v3.5.3).
    /// Unlike its siblings it creates only in write contexts, and updates entries in place
    /// (same lengths) in any other mode.
    pub fn attr_set_string_vector(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        size: i32,
        val: Option<&[Option<&str>]>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        if size < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid size (%d) for string vector '%s'",
                &[size.into(), nm(name)],
            ));
        }
        let Some(val) = val else {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "No input string values for setting '%s', type 'stringvector'",
                &[nm(name)],
            ));
        };
        // a slice shorter than `size` is refused where upstream would read past its end
        // (docs/improvements.md, U-2)
        let short = val.len() < size as usize;
        let write = self.mode == ContextMode::Write;
        let id = match self.find_attr_id(part_index, name)? {
            None => {
                if !write {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let id = self.add_attr(part_index, name, AttributeType::STRING_VECTOR)?;
                let Context { core, parts, .. } = self;
                let AttrValue::StringVector(sv) = &mut parts[part_index as usize]
                    .attributes
                    .get_mut(id)
                    .expect("attr")
                    .value
                else {
                    unreachable!("added as a string vector")
                };
                core.attr_string_vector_init(sv, size)?;
                if short {
                    return Err(core.standard_error(ErrorCode::InvalidArgument));
                }
                for (i, v) in val.iter().take(size as usize).enumerate() {
                    core.attr_string_vector_set_entry(sv, i as i32, v.map(str::as_bytes))?;
                }
                return Ok(());
            }
            Some(id) => id,
        };
        let (at, tn) = self.attr_type_of(part_index, id);
        if at != AttributeType::STRING_VECTOR {
            return Err(self.print_error(
                ErrorCode::AttrTypeMismatch,
                "'%s' requested type 'stringvector', but attribute is type '%s'",
                &[nm(name), Arg::Str(&tn)],
            ));
        }
        let Context { core, parts, .. } = self;
        let AttrValue::StringVector(sv) = &mut parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("attr")
            .value
        else {
            unreachable!("checked")
        };
        if sv.n_strings == size && sv.alloc_size > 0 {
            if short {
                return Err(core.standard_error(ErrorCode::InvalidArgument));
            }
            if !write {
                for (i, v) in val.iter().take(size as usize).enumerate() {
                    let Some(v) = v else {
                        return Err(core.print_error(
                            ErrorCode::InvalidArgument,
                            "'%s' received NULL string in string vector",
                            &[nm(name)],
                        ));
                    };
                    let curlen = v.len();
                    let oldlen = sv.strings[i].length;
                    if curlen != oldlen as usize {
                        return Err(core.print_error(
                            ErrorCode::InvalidArgument,
                            "'%s' string %d in string vector is different size (old %d new %d), unable to update",
                            &[nm(name), (i as i32).into(), oldlen.into(), (curlen as i32).into()],
                        ));
                    }
                    core.attr_string_vector_set_entry_with_length(
                        sv,
                        i as i32,
                        Some(v.as_bytes()),
                        curlen as i32,
                    )?;
                }
            } else {
                for (i, v) in val.iter().take(size as usize).enumerate() {
                    core.attr_string_vector_set_entry(sv, i as i32, v.map(str::as_bytes))?;
                }
            }
            Ok(())
        } else if !write {
            let n = sv.n_strings;
            Err(core.print_error(
                ErrorCode::ModifySizeChange,
                "Existing string vector '%s' has %d strings, but given %d, unable to change",
                &[nm(name), n.into(), size.into()],
            ))
        } else {
            if short {
                return Err(core.standard_error(ErrorCode::InvalidArgument));
            }
            for (i, v) in val.iter().take(size as usize).enumerate() {
                core.attr_string_vector_set_entry(sv, i as i32, v.map(str::as_bytes))?;
            }
            Ok(())
        }
    }

    /// Port of `exr_attr_set_user` (src/lib/OpenEXRCore/part_attr.c:2506-2558 @ v3.5.3): an
    /// opaque attribute of type `type_`, set unpacked (and packed) when its type has a pack
    /// function, else from packed bytes. A failed add, or one that made a built-in type, is
    /// refused (docs/improvements.md, U-5).
    pub fn attr_set_user(
        &mut self,
        part_index: i32,
        name: Option<&str>,
        type_: Option<&str>,
        size: i32,
        unpacked: Option<Unpacked>,
        packed: Option<&[u8]>,
    ) -> Result<(), Error> {
        self.part_ref(part_index)?;
        self.setter_mode_check()?;
        let id = match self.find_attr_id(part_index, name)? {
            None => {
                if self.mode != ContextMode::Write {
                    return Err(Error::bare(ErrorCode::NoAttrByName));
                }
                let Context {
                    core,
                    parts,
                    custom_handlers,
                    ..
                } = self;
                let part = &mut parts[part_index as usize];
                let (id, _) = core.attr_list_add_by_type(
                    custom_handlers,
                    &mut part.attributes,
                    name.map(str::as_bytes),
                    type_.map(str::as_bytes),
                    0,
                    false,
                )?;
                id
            }
            Some(id) => {
                let (at, tn) = self.attr_type_of(part_index, id);
                if at != AttributeType::OPAQUE {
                    return Err(self.print_error(
                        ErrorCode::AttrTypeMismatch,
                        "'%s' requested type '%s', but stored attributes is type '%s'",
                        &[nm(name), nm(type_), Arg::Str(&tn)],
                    ));
                }
                id
            }
        };
        let Context { core, parts, .. } = self;
        let AttrValue::Opaque(opq) = &mut parts[part_index as usize]
            .attributes
            .get_mut(id)
            .expect("attr")
            .value
        else {
            return Err(core.standard_error(ErrorCode::AttrTypeMismatch));
        };
        if opq.pack_func_ptr.is_some() {
            core.attr_opaquedata_set_unpacked(opq, unpacked, size)?;
            core.attr_opaquedata_pack(opq).map(|_| ())
        } else {
            core.attr_opaquedata_set_packed(opq, packed, size)
        }
    }
}
