// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `attributes.c` (src/lib/OpenEXRCore/attributes.c:1-1133 @ v3.5.3), with
//! `exr_attribute_t` (src/lib/OpenEXRCore/openexr_attr.h:463-515 @ v3.5.3) and
//! `exr_attribute_list_t` (src/lib/OpenEXRCore/internal_attr.h:46-53 @ v3.5.3).
//!
//! An attribute is a name, a type name and a value; C's union is [`AttrValue`], whose variant
//! is the attribute's type. A list keeps its attributes in creation order and an index sorted
//! by name, as upstream's two pointer arrays do. Where C hands out `exr_attribute_t*`, the
//! port hands out an [`AttrId`], stable for the attribute's life in its list. Upstream's
//! single allocation block (attribute, names, value, extra data) has no counterpart beyond
//! what it makes observable: the extra data a caller asked for ([`Attribute::extra`]).

// Ported ahead of its callers (header parsing and the part attribute API: Phase 3).
#![allow(dead_code)]

use crate::bytes::AttrBytes;
use crate::cfmt::Arg;
use crate::channel_list::AttrChlist;
use crate::context::{ContextCore, Error};
use crate::errors::ErrorCode;
use crate::float_vector::AttrFloatVector;
use crate::opaque::AttrOpaqueData;
use crate::openexr_attr::*;
use crate::preview::AttrPreview;
use crate::string::AttrString;
use crate::string_vector::AttrStringVector;

/// The value of an attribute: the active member of `exr_attribute_t`'s union.
#[derive(Debug, Clone)]
pub enum AttrValue {
    Box2i(Box2i),
    Box2f(Box2f),
    Bytes(AttrBytes),
    Chlist(AttrChlist),
    Chromaticities(Chromaticities),
    /// `uc`
    Compression(u8),
    /// `d`
    Double(f64),
    /// `uc`
    Envmap(u8),
    /// `f`
    Float(f32),
    FloatVector(AttrFloatVector),
    /// `i`
    Int(i32),
    Keycode(Keycode),
    /// `uc`
    LineOrder(u8),
    M33f(M33f),
    M33d(M33d),
    M44f(M44f),
    M44d(M44d),
    Preview(AttrPreview),
    Rational(Rational),
    String(AttrString),
    StringVector(AttrStringVector),
    TileDesc(TileDesc),
    Timecode(Timecode),
    V2i(V2i),
    V2f(V2f),
    V2d(V2d),
    V3i(V3i),
    V3f(V3f),
    V3d(V3d),
    /// `uc`
    DeepImageState(u8),
    Opaque(AttrOpaqueData),
}

impl AttrValue {
    /// The `type` field the value goes with.
    pub fn attr_type(&self) -> AttributeType {
        match self {
            AttrValue::Box2i(_) => AttributeType::BOX2I,
            AttrValue::Box2f(_) => AttributeType::BOX2F,
            AttrValue::Bytes(_) => AttributeType::BYTES,
            AttrValue::Chlist(_) => AttributeType::CHLIST,
            AttrValue::Chromaticities(_) => AttributeType::CHROMATICITIES,
            AttrValue::Compression(_) => AttributeType::COMPRESSION,
            AttrValue::Double(_) => AttributeType::DOUBLE,
            AttrValue::Envmap(_) => AttributeType::ENVMAP,
            AttrValue::Float(_) => AttributeType::FLOAT,
            AttrValue::FloatVector(_) => AttributeType::FLOAT_VECTOR,
            AttrValue::Int(_) => AttributeType::INT,
            AttrValue::Keycode(_) => AttributeType::KEYCODE,
            AttrValue::LineOrder(_) => AttributeType::LINEORDER,
            AttrValue::M33f(_) => AttributeType::M33F,
            AttrValue::M33d(_) => AttributeType::M33D,
            AttrValue::M44f(_) => AttributeType::M44F,
            AttrValue::M44d(_) => AttributeType::M44D,
            AttrValue::Preview(_) => AttributeType::PREVIEW,
            AttrValue::Rational(_) => AttributeType::RATIONAL,
            AttrValue::String(_) => AttributeType::STRING,
            AttrValue::StringVector(_) => AttributeType::STRING_VECTOR,
            AttrValue::TileDesc(_) => AttributeType::TILEDESC,
            AttrValue::Timecode(_) => AttributeType::TIMECODE,
            AttrValue::V2i(_) => AttributeType::V2I,
            AttrValue::V2f(_) => AttributeType::V2F,
            AttrValue::V2d(_) => AttributeType::V2D,
            AttrValue::V3i(_) => AttributeType::V3I,
            AttrValue::V3f(_) => AttributeType::V3F,
            AttrValue::V3d(_) => AttributeType::V3D,
            AttrValue::DeepImageState(_) => AttributeType::DEEP_IMAGE_STATE,
            AttrValue::Opaque(_) => AttributeType::OPAQUE,
        }
    }
}

/// A stable handle to an attribute of a list: what upstream's `exr_attribute_t*` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AttrId(u32);

/// Port of `exr_attribute_t` (src/lib/OpenEXRCore/openexr_attr.h:463-515 @ v3.5.3).
#[derive(Debug, Clone)]
pub struct Attribute {
    id: AttrId,
    /// `name`, without its NUL.
    pub(crate) name: Vec<u8>,
    /// `type_name`, without its NUL.
    pub(crate) type_name: Vec<u8>,
    pub(crate) name_length: u8,
    pub(crate) type_name_length: u8,
    pub(crate) value: AttrValue,
    /// The extra storage the caller asked for with `data_len` (zeroed; upstream's is
    /// uninitialised).
    pub(crate) extra: Option<Vec<u8>>,
}

impl Attribute {
    /// The attribute's handle in its list.
    pub fn id(&self) -> AttrId {
        self.id
    }

    /// The `name` field.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// The `type_name` field.
    pub fn type_name(&self) -> &[u8] {
        &self.type_name
    }

    /// The `name_length` field.
    pub fn name_length(&self) -> u8 {
        self.name_length
    }

    /// The `type_name_length` field.
    pub fn type_name_length(&self) -> u8 {
        self.type_name_length
    }

    /// The `type` field.
    pub fn attr_type(&self) -> AttributeType {
        self.value.attr_type()
    }

    /// The value.
    pub fn value(&self) -> &AttrValue {
        &self.value
    }

    /// The value, to fill in.
    pub fn value_mut(&mut self) -> &mut AttrValue {
        &mut self.value
    }

    /// The extra storage, if the caller asked for some.
    pub fn extra(&self) -> Option<&[u8]> {
        self.extra.as_deref()
    }
}

/// Port of `exr_attribute_list_t` (src/lib/OpenEXRCore/internal_attr.h:46-53 @ v3.5.3).
#[derive(Debug, Clone, Default)]
pub struct AttributeList {
    pub(crate) num_attributes: i32,
    pub(crate) num_alloced: i32,
    /// `entries`: creation order.
    entries: Vec<Attribute>,
    /// `sorted_entries`: positions in `entries`, in `strcmp` order of the names.
    sorted: Vec<usize>,
    next_id: u32,
}

impl AttributeList {
    /// The `num_attributes` field.
    pub fn num_attributes(&self) -> i32 {
        self.num_attributes
    }

    /// The `num_alloced` field.
    pub fn num_alloced(&self) -> i32 {
        self.num_alloced
    }

    /// The attributes in creation order (`entries`).
    pub fn entries(&self) -> &[Attribute] {
        &self.entries
    }

    /// The attributes in name order (`sorted_entries`).
    pub fn sorted_entries(&self) -> impl Iterator<Item = &Attribute> {
        self.sorted.iter().map(|&i| &self.entries[i])
    }

    /// The indices of `entries` in name order (`sorted_entries`).
    pub(crate) fn sorted_order(&self) -> &[usize] {
        &self.sorted
    }

    /// The attributes in creation order, to change (the header writer packs opaque data).
    pub(crate) fn entries_mut(&mut self) -> &mut [Attribute] {
        &mut self.entries
    }

    /// The attribute `id`, if it is in the list.
    pub fn get(&self, id: AttrId) -> Option<&Attribute> {
        self.entries.iter().find(|a| a.id == id)
    }

    /// The attribute `id`, if it is in the list, to change.
    pub fn get_mut(&mut self, id: AttrId) -> Option<&mut Attribute> {
        self.entries.iter_mut().find(|a| a.id == id)
    }
}

/// `the_predefined_attr_typenames` (src/lib/OpenEXRCore/attributes.c:13-59 @ v3.5.3): the
/// type names, their lengths and types, in `exr_attribute_type_t` order.
static THE_PREDEFINED_ATTR_TYPENAMES: [(&str, u32, AttributeType); 30] = [
    ("box2i", 5, AttributeType::BOX2I),
    ("box2f", 5, AttributeType::BOX2F),
    ("bytes", 5, AttributeType::BYTES),
    ("chlist", 6, AttributeType::CHLIST),
    ("chromaticities", 14, AttributeType::CHROMATICITIES),
    ("compression", 11, AttributeType::COMPRESSION),
    ("double", 6, AttributeType::DOUBLE),
    ("envmap", 6, AttributeType::ENVMAP),
    ("float", 5, AttributeType::FLOAT),
    ("floatvector", 11, AttributeType::FLOAT_VECTOR),
    ("int", 3, AttributeType::INT),
    ("keycode", 7, AttributeType::KEYCODE),
    ("lineOrder", 9, AttributeType::LINEORDER),
    ("m33f", 4, AttributeType::M33F),
    ("m33d", 4, AttributeType::M33D),
    ("m44f", 4, AttributeType::M44F),
    ("m44d", 4, AttributeType::M44D),
    ("preview", 7, AttributeType::PREVIEW),
    ("rational", 8, AttributeType::RATIONAL),
    ("string", 6, AttributeType::STRING),
    ("stringvector", 12, AttributeType::STRING_VECTOR),
    ("tiledesc", 8, AttributeType::TILEDESC),
    ("timecode", 8, AttributeType::TIMECODE),
    ("v2i", 3, AttributeType::V2I),
    ("v2f", 3, AttributeType::V2F),
    ("v2d", 3, AttributeType::V2D),
    ("v3i", 3, AttributeType::V3I),
    ("v3f", 3, AttributeType::V3F),
    ("v3d", 3, AttributeType::V3D),
    ("deepImageState", 14, AttributeType::DEEP_IMAGE_STATE),
];

/// `sizeof` of the C structs `exr_attr_list_compute_size` adds, on x86-64 (the reference
/// platforms): `exr_attr_bytes_t` is `size_t`, a pointer, `uint32_t`, padding and a pointer.
const SIZEOF_EXR_ATTR_BYTES_T: u64 = 32;

/// The bytes of a C string up to its NUL.
fn c_str(s: &[u8]) -> &[u8] {
    &s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())]
}

/// Port of `attr_init` (src/lib/OpenEXRCore/attributes.c:66-212 @ v3.5.3): the zero value of
/// a known type. `EXR_ATTR_M44D` is zeroed as an `exr_attr_m44f_t` upstream, leaving its last
/// eight doubles as allocated; the port zeroes all sixteen (docs/improvements.md, U-3).
fn attr_init(t: AttributeType) -> Option<AttrValue> {
    Some(match t {
        AttributeType::BOX2I => AttrValue::Box2i(Box2i::default()),
        AttributeType::BOX2F => AttrValue::Box2f(Box2f::default()),
        AttributeType::BYTES => AttrValue::Bytes(AttrBytes::default()),
        AttributeType::CHLIST => AttrValue::Chlist(AttrChlist::default()),
        AttributeType::CHROMATICITIES => AttrValue::Chromaticities(Chromaticities::default()),
        AttributeType::COMPRESSION => AttrValue::Compression(0),
        AttributeType::ENVMAP => AttrValue::Envmap(0),
        AttributeType::LINEORDER => AttrValue::LineOrder(0),
        AttributeType::DEEP_IMAGE_STATE => AttrValue::DeepImageState(0),
        AttributeType::DOUBLE => AttrValue::Double(0.0),
        AttributeType::FLOAT => AttrValue::Float(0.0),
        AttributeType::FLOAT_VECTOR => AttrValue::FloatVector(AttrFloatVector::default()),
        AttributeType::INT => AttrValue::Int(0),
        AttributeType::KEYCODE => AttrValue::Keycode(Keycode::default()),
        AttributeType::M33F => AttrValue::M33f(M33f::default()),
        AttributeType::M33D => AttrValue::M33d(M33d::default()),
        AttributeType::M44F => AttrValue::M44f(M44f::default()),
        AttributeType::M44D => AttrValue::M44d(M44d::default()),
        AttributeType::PREVIEW => AttrValue::Preview(AttrPreview::default()),
        AttributeType::RATIONAL => AttrValue::Rational(Rational::default()),
        AttributeType::STRING => AttrValue::String(AttrString::default()),
        AttributeType::STRING_VECTOR => AttrValue::StringVector(AttrStringVector::default()),
        AttributeType::TILEDESC => AttrValue::TileDesc(TileDesc::default()),
        AttributeType::TIMECODE => AttrValue::Timecode(Timecode::default()),
        AttributeType::V2I => AttrValue::V2i(V2i::default()),
        AttributeType::V2F => AttrValue::V2f(V2f::default()),
        AttributeType::V2D => AttrValue::V2d(V2d::default()),
        AttributeType::V3I => AttrValue::V3i(V3i::default()),
        AttributeType::V3F => AttrValue::V3f(V3f::default()),
        AttributeType::V3D => AttrValue::V3d(V3d::default()),
        AttributeType::OPAQUE => AttrValue::Opaque(AttrOpaqueData::default()),
        _ => return None,
    })
}

/// Port of `internal_exr_is_standard_type` (src/lib/OpenEXRCore/attributes.c:277-287 @ v3.5.3).
pub(crate) fn internal_exr_is_standard_type(typen: &[u8]) -> bool {
    let typen = c_str(typen);
    THE_PREDEFINED_ATTR_TYPENAMES
        .iter()
        .any(|(n, _, _)| n.as_bytes() == typen)
}

/// What `validate_attr_arguments` decided.
enum Validated {
    /// Not in the list: go on and add it.
    Add,
    /// Already in the list (upstream's `-1`).
    Exists(AttrId),
}

impl ContextCore {
    /// Port of `exr_attr_list_destroy` (src/lib/OpenEXRCore/attributes.c:290-314 @ v3.5.3).
    pub(crate) fn attr_list_destroy(&self, list: &mut AttributeList) {
        for attr in &mut list.entries {
            attr_destroy(self, attr);
        }
        *list = AttributeList::default();
    }

    /// Port of `exr_attr_list_compute_size` (src/lib/OpenEXRCore/attributes.c:317-425 @ v3.5.3):
    /// the bytes the list takes in a file header, as upstream counts them (a bytes attribute
    /// counts `sizeof (exr_attr_bytes_t)` plus its data; an opaque one is packed to count it).
    pub(crate) fn attr_list_compute_size(&self, list: &mut AttributeList) -> Result<u64, Error> {
        let mut retval: u64 = 0;
        for cur in list.entries.iter_mut() {
            retval += u64::from(cur.name_length) + 1;
            retval += u64::from(cur.type_name_length) + 1;
            retval += 4;
            retval += match &mut cur.value {
                AttrValue::Box2i(_) | AttrValue::Box2f(_) => 16,
                AttrValue::Bytes(b) => {
                    SIZEOF_EXR_ATTR_BYTES_T + if b.data.is_some() { b.size as u64 } else { 0 }
                }
                AttrValue::Chlist(cl) => cl
                    .entries
                    .iter()
                    .take(cl.num_channels.max(0) as usize)
                    .map(|e| (e.name.length as u64).wrapping_add(1) + 4 * 4)
                    .sum(),
                AttrValue::Chromaticities(_) => 32,
                AttrValue::Compression(_) | AttrValue::Envmap(_) | AttrValue::LineOrder(_) => 1,
                AttrValue::Double(_) => 8,
                AttrValue::Float(_) => 4,
                AttrValue::FloatVector(fv) => 4u64.wrapping_mul(fv.length as i64 as u64),
                AttrValue::Int(_) => 4,
                AttrValue::Keycode(_) => 28,
                AttrValue::M33f(_) => 36,
                AttrValue::M33d(_) => 72,
                AttrValue::M44f(_) => 64,
                AttrValue::M44d(_) => 128,
                AttrValue::Preview(p) => u64::from(p.width) * u64::from(p.height) * 4,
                AttrValue::Rational(_) => 8,
                AttrValue::String(s) => s.length as i64 as u64,
                AttrValue::StringVector(sv) => sv
                    .strings
                    .iter()
                    .take(sv.n_strings.max(0) as usize)
                    .map(|s| (s.length as i64 as u64).wrapping_add(4))
                    .sum(),
                AttrValue::TileDesc(_) => 9,
                AttrValue::Timecode(_) => 8,
                AttrValue::V2i(_) | AttrValue::V2f(_) => 8,
                AttrValue::V2d(_) => 16,
                AttrValue::V3i(_) | AttrValue::V3f(_) => 12,
                AttrValue::V3d(_) => 24,
                AttrValue::DeepImageState(_) => 1,
                AttrValue::Opaque(o) => {
                    if o.packed_data.is_some() {
                        o.size as i64 as u64
                    } else if o.unpacked_data.is_some() {
                        let (sz, _) = self.attr_opaquedata_pack(o)?;
                        sz as i64 as u64
                    } else {
                        0
                    }
                }
            };
        }
        Ok(retval)
    }

    /// Port of `exr_attr_list_find_by_name` (src/lib/OpenEXRCore/attributes.c:427-497 @ v3.5.3):
    /// a lower-bound search of the sorted entries. `EXR_ERR_NO_ATTR_BY_NAME` is returned
    /// without a report, as upstream does.
    pub(crate) fn attr_list_find_by_name(
        &self,
        list: &AttributeList,
        name: Option<&[u8]>,
    ) -> Result<AttrId, Error> {
        let name = match name.map(c_str) {
            Some(n) if !n.is_empty() => n,
            _ => {
                return Err(self.report_error(
                    ErrorCode::InvalidArgument,
                    b"Invalid name passed to find_by_name",
                ));
            }
        };
        if !list.sorted.is_empty() {
            let at = |k: usize| &list.entries[list.sorted[k]];
            let mut first = 0usize;
            let mut count = list.num_attributes.max(0) as usize;
            let end = count;
            // lower bound search w/ equality check
            while count > 0 {
                let step = count / 2;
                let it = first + step;
                match at(it).name.as_slice().cmp(name) {
                    std::cmp::Ordering::Equal => return Ok(at(it).id), // early exit
                    std::cmp::Ordering::Less => {
                        first = it + 1;
                        count -= step + 1;
                    }
                    std::cmp::Ordering::Greater => count = step,
                }
            }
            if first < end && at(first).name.as_slice() == name {
                return Ok(at(first).id);
            }
        }
        Err(Error::bare(ErrorCode::NoAttrByName))
    }

    /// Port of `validate_attr_arguments` (src/lib/OpenEXRCore/attributes.c:564-640 @ v3.5.3).
    /// `data_ptr` says whether the caller passed upstream's `data_ptr`.
    fn validate_attr_arguments(
        &self,
        list: &AttributeList,
        name: Option<&[u8]>,
        data_len: i32,
        data_ptr: bool,
    ) -> Result<Validated, Error> {
        if data_len < 0 {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Extra data storage requested negative length (%d)",
                &[data_len.into()],
            ));
        } else if data_len > 0 && !data_ptr {
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Extra data storage output pointer must be provided when requesting extra data (%d)",
                &[data_len.into()],
            ));
        }
        let name = match name.map(c_str) {
            Some(n) if !n.is_empty() => n,
            _ => {
                return Err(
                    self.report_error(ErrorCode::InvalidArgument, b"Invalid name to add_by_type")
                );
            }
        };
        // is it already in the list?
        if let Ok(id) = self.attr_list_find_by_name(list, Some(name)) {
            if data_ptr && data_len > 0 {
                let nattr = list.get(id).expect("found");
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Attribute '%s' (type %s) already in list but requesting additional data",
                    &[Arg::Str(name), Arg::Str(&nattr.type_name)],
                ));
            }
            return Ok(Validated::Exists(id));
        }
        Ok(Validated::Add)
    }

    /// Port of `add_to_list` (src/lib/OpenEXRCore/attributes.c:499-562 @ v3.5.3): appends,
    /// bubbles the new entry into name order, and grows the arrays to twice their size or one
    /// past the new count. `attr_init` cannot fail for the types that reach it.
    fn add_to_list(
        &self,
        list: &mut AttributeList,
        name: Vec<u8>,
        type_name: Vec<u8>,
        name_length: u8,
        type_name_length: u8,
        value: AttrValue,
        extra: Option<Vec<u8>>,
    ) -> AttrId {
        let cattrsz = list.num_attributes;
        let nattrsz = cattrsz + 1;
        if nattrsz > list.num_alloced {
            let mut nsize = (list.num_alloced as i64 as usize).wrapping_mul(2);
            if nattrsz as usize > nsize {
                nsize = nattrsz as usize + 1;
            }
            list.num_alloced = nsize as i32;
        }
        let id = AttrId(list.next_id);
        list.next_id += 1;
        let pos = list.entries.len();
        list.entries.push(Attribute {
            id,
            name,
            type_name,
            name_length,
            type_name_length,
            value,
            extra,
        });
        // FYI: qsort is shockingly slow, just do a quick search and bubble it up until it's
        // in the correct location
        let mut insertpos = list.sorted.len();
        while insertpos > 0 {
            let prev = &list.entries[list.sorted[insertpos - 1]];
            if list.entries[pos].name.as_slice() >= prev.name.as_slice() {
                break;
            }
            insertpos -= 1;
        }
        list.sorted.insert(insertpos, pos);
        list.num_attributes = nattrsz;
        id
    }

    /// Port of `check_attr_handler` (src/lib/OpenEXRCore/attributes.c:642-660 @ v3.5.3): an
    /// opaque attribute takes the pack and unpack functions registered for its type.
    fn check_attr_handler(&self, handlers: &AttributeList, attr: &mut Attribute) {
        if let AttrValue::Opaque(o) = &mut attr.value
            && let Ok(hid) = self.attr_list_find_by_name(handlers, Some(&attr.type_name))
            && let Some(AttrValue::Opaque(h)) = handlers.get(hid).map(|h| &h.value)
        {
            o.unpack_func_ptr = h.unpack_func_ptr.clone();
            o.pack_func_ptr = h.pack_func_ptr.clone();
            o.destroy_unpacked_func_ptr = h.destroy_unpacked_func_ptr.clone();
        }
    }

    /// The name-length checks the three add functions share.
    fn check_name_length(&self, what: &str, name: &[u8]) -> Result<u8, Error> {
        let mlen = i32::from(self.max_name_length);
        if name.len() > mlen as usize {
            let fmt = if what == "type" {
                "Provided type name '%s' too long for file (len %d, max %d)"
            } else {
                "Provided name '%s' too long for file (len %d, max %d)"
            };
            return Err(self.print_error(
                ErrorCode::NameTooLong,
                fmt,
                &[Arg::Str(name), (name.len() as i32).into(), mlen.into()],
            ));
        }
        Ok(name.len() as u8)
    }

    /// Port of `exr_attr_list_add_by_type` (src/lib/OpenEXRCore/attributes.c:751-882 @ v3.5.3):
    /// adds `name` with the type named `type_`, a built-in one or else opaque. Returns the
    /// attribute (an existing one of the same type, unchanged) and whether extra storage was
    /// made (`*data_ptr != NULL`).
    pub(crate) fn attr_list_add_by_type(
        &self,
        handlers: &AttributeList,
        list: &mut AttributeList,
        name: Option<&[u8]>,
        type_: Option<&[u8]>,
        data_len: i32,
        data_ptr: bool,
    ) -> Result<(AttrId, bool), Error> {
        let ty = match type_.map(c_str) {
            Some(t) if !t.is_empty() => t,
            _ => {
                return Err(
                    self.report_error(ErrorCode::InvalidArgument, b"Invalid type to add_by_type")
                );
            }
        };
        match self.validate_attr_arguments(list, name, data_len, data_ptr)? {
            Validated::Exists(id) => {
                let existing = list.get(id).expect("found");
                if existing.type_name != ty {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Entry '%s' already in list but with different type ('%s' vs requested '%s')",
                        &[
                            Arg::Str(c_str(name.unwrap_or_default())),
                            Arg::Str(&existing.type_name),
                            Arg::Str(ty),
                        ],
                    ));
                }
                return Ok((id, false));
            }
            Validated::Add => {}
        }
        let name = c_str(name.unwrap_or_default());
        let nlen = self.check_name_length("name", name)?;
        let tlen = self.check_name_length("type", ty)?;
        let known = THE_PREDEFINED_ATTR_TYPENAMES
            .iter()
            .find(|(n, _, _)| n.as_bytes() == ty);
        let extra = (data_ptr && data_len > 0).then(|| vec![0u8; data_len as usize]);
        let has_extra = extra.is_some();
        let (type_name, type_name_length, value) = match known {
            Some((n, len, t)) => (
                n.as_bytes().to_vec(),
                *len as u8,
                attr_init(*t).expect("a known type"),
            ),
            None => (
                ty.to_vec(),
                tlen,
                AttrValue::Opaque(AttrOpaqueData::default()),
            ),
        };
        let id = self.add_to_list(
            list,
            name.to_vec(),
            type_name,
            nlen,
            type_name_length,
            value,
            extra,
        );
        let attr = list.get_mut(id).expect("added");
        self.check_attr_handler(handlers, attr);
        Ok((id, has_extra))
    }

    /// Port of `exr_attr_list_add` (src/lib/OpenEXRCore/attributes.c:884-979 @ v3.5.3): adds
    /// `name` with a built-in type.
    pub(crate) fn attr_list_add(
        &self,
        handlers: &AttributeList,
        list: &mut AttributeList,
        name: Option<&[u8]>,
        type_: AttributeType,
        data_len: i32,
        data_ptr: bool,
    ) -> Result<(AttrId, bool), Error> {
        self.attr_list_add_builtin(handlers, list, name, type_, data_len, data_ptr)
    }

    /// Port of `exr_attr_list_add_static_name` (src/lib/OpenEXRCore/attributes.c:981-1077
    /// @ v3.5.3): as `exr_attr_list_add`, but the name is not copied (here it is: the
    /// observable state is the same).
    pub(crate) fn attr_list_add_static_name(
        &self,
        handlers: &AttributeList,
        list: &mut AttributeList,
        name: Option<&[u8]>,
        type_: AttributeType,
        data_len: i32,
        data_ptr: bool,
    ) -> Result<(AttrId, bool), Error> {
        self.attr_list_add_builtin(handlers, list, name, type_, data_len, data_ptr)
    }

    /// The common body of `exr_attr_list_add` and `exr_attr_list_add_static_name`, which differ
    /// only in whether the name is copied into the attribute's block.
    fn attr_list_add_builtin(
        &self,
        handlers: &AttributeList,
        list: &mut AttributeList,
        name: Option<&[u8]>,
        type_: AttributeType,
        data_len: i32,
        data_ptr: bool,
    ) -> Result<(AttrId, bool), Error> {
        match self.validate_attr_arguments(list, name, data_len, data_ptr)? {
            Validated::Exists(id) => {
                let existing = list.get(id).expect("found");
                if existing.attr_type() != type_ {
                    return Err(self.print_error(
                        ErrorCode::InvalidArgument,
                        "Entry '%s' already in list but with different type ('%s')",
                        &[
                            Arg::Str(c_str(name.unwrap_or_default())),
                            Arg::Str(&existing.type_name),
                        ],
                    ));
                }
                return Ok((id, false));
            }
            Validated::Add => {}
        }
        let name = c_str(name.unwrap_or_default());
        let nlen = self.check_name_length("name", name)?;
        let tidx = type_.0.wrapping_sub(1);
        if tidx < 0 || tidx >= THE_PREDEFINED_ATTR_TYPENAMES.len() as i32 {
            if type_ == AttributeType::OPAQUE {
                return Err(self.print_error(
                    ErrorCode::InvalidArgument,
                    "Invalid type enum for '%s': the opaque type is not actually a built-in type",
                    &[Arg::Str(name)],
                ));
            }
            return Err(self.print_error(
                ErrorCode::InvalidArgument,
                "Invalid type enum for '%s' in create by builtin type (type %d)",
                &[Arg::Str(name), type_.0.into()],
            ));
        }
        let (n, len, t) = THE_PREDEFINED_ATTR_TYPENAMES[tidx as usize];
        let extra = (data_ptr && data_len > 0).then(|| vec![0u8; data_len as usize]);
        let has_extra = extra.is_some();
        let value = attr_init(t).expect("a known type");
        let id = self.add_to_list(
            list,
            name.to_vec(),
            n.as_bytes().to_vec(),
            nlen,
            len as u8,
            value,
            extra,
        );
        let attr = list.get_mut(id).expect("added");
        self.check_attr_handler(handlers, attr);
        Ok((id, has_extra))
    }

    /// Port of `exr_attr_list_remove` (src/lib/OpenEXRCore/attributes.c:1079-1133 @ v3.5.3).
    pub(crate) fn attr_list_remove(
        &self,
        list: &mut AttributeList,
        attr: Option<AttrId>,
    ) -> Result<(), Error> {
        let Some(attr) = attr else {
            return Err(self.report_error(
                ErrorCode::InvalidArgument,
                b"NULL attribute passed to remove",
            ));
        };
        let Some(attridx) = list.entries.iter().position(|a| a.id == attr) else {
            return Err(self.report_error(ErrorCode::InvalidArgument, b"Attribute not in list"));
        };
        let mut removed = list.entries.remove(attridx);
        list.num_attributes -= 1;
        list.sorted.retain(|&p| p != attridx);
        for p in &mut list.sorted {
            if *p > attridx {
                *p -= 1;
            }
        }
        attr_destroy(self, &mut removed);
        Ok(())
    }
}

/// Port of `attr_destroy` (src/lib/OpenEXRCore/attributes.c:214-275 @ v3.5.3): frees what the
/// value owns (and, for opaque data, calls its destroy function).
fn attr_destroy(ctxt: &ContextCore, attr: &mut Attribute) {
    match &mut attr.value {
        AttrValue::Chlist(c) => ctxt.attr_chlist_destroy(c),
        AttrValue::FloatVector(f) => ctxt.attr_float_vector_destroy(f),
        AttrValue::Preview(p) => ctxt.attr_preview_destroy(p),
        AttrValue::String(s) => ctxt.attr_string_destroy(s),
        AttrValue::StringVector(s) => ctxt.attr_string_vector_destroy(s),
        AttrValue::Opaque(o) => ctxt.attr_opaquedata_destroy(o),
        AttrValue::Bytes(b) => ctxt.attr_bytes_destroy(b),
        _ => {}
    }
}
