// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Attribute values: one variant for each of Core's attribute types, each kept as the file
//! holds it (nothing folded: a `float` stays apart from a `double`, a string stays its bytes,
//! an enumerated value Core does not know stays its byte).

use openexr_core::attributes::AttrValue;

/// A 2D vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V2<T> {
    pub x: T,
    pub y: T,
}

/// A 3D vector.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V3<T> {
    pub x: T,
    pub y: T,
    pub z: T,
}

/// An axis-aligned box, its corners included.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Box2<T> {
    pub min: V2<T>,
    pub max: V2<T>,
}

pub type V2i = V2<i32>;
pub type V2f = V2<f32>;
pub type V2d = V2<f64>;
pub type V3i = V3<i32>;
pub type V3f = V3<f32>;
pub type V3d = V3<f64>;
pub type Box2i = Box2<i32>;
pub type Box2f = Box2<f32>;

impl Box2i {
    /// The number of columns, `max.x - min.x + 1` (0 for an empty box).
    pub fn width(&self) -> u64 {
        (i64::from(self.max.x) - i64::from(self.min.x) + 1).max(0) as u64
    }

    /// The number of rows, `max.y - min.y + 1` (0 for an empty box).
    pub fn height(&self) -> u64 {
        (i64::from(self.max.y) - i64::from(self.min.y) + 1).max(0) as u64
    }
}

/// A channel's sample type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelType {
    /// 32-bit unsigned integers.
    Uint,
    /// 16-bit floats.
    Half,
    /// 32-bit floats.
    Float,
    /// A value Core does not know.
    Other(i32),
}

impl PixelType {
    pub(crate) fn from_core(t: openexr_core::openexr_attr::PixelType) -> PixelType {
        match t.0 {
            0 => PixelType::Uint,
            1 => PixelType::Half,
            2 => PixelType::Float,
            v => PixelType::Other(v),
        }
    }

    /// Core's value for the type.
    pub fn value(self) -> i32 {
        match self {
            PixelType::Uint => 0,
            PixelType::Half => 1,
            PixelType::Float => 2,
            PixelType::Other(v) => v,
        }
    }

    /// The bytes one sample takes (4 for an unknown type).
    pub fn size(self) -> usize {
        if self == PixelType::Half { 2 } else { 4 }
    }
}

macro_rules! byte_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $var:ident = $val:expr),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($(#[$vdoc])* $var,)*
            /// A value Core does not know, as the file holds it.
            Other(u8),
        }

        impl $name {
            /// The value as the file holds it.
            pub fn value(self) -> u8 {
                match self {
                    $($name::$var => $val,)*
                    $name::Other(v) => v,
                }
            }

            /// The value `v`.
            pub fn from_value(v: u8) -> $name {
                match v {
                    $($val => $name::$var,)*
                    v => $name::Other(v),
                }
            }
        }
    };
}

byte_enum! {
    /// A part's compression.
    Compression {
        None = 0, Rle = 1, Zips = 2, Zip = 3, Piz = 4, Pxr24 = 5, B44 = 6, B44a = 7,
        Dwaa = 8, Dwab = 9, Htj2k256 = 10, Htj2k32 = 11, Lj2k = 12, Zstd = 13,
    }
}

byte_enum! {
    /// The order a part's scanlines (or tiles) are stored in.
    LineOrder { IncreasingY = 0, DecreasingY = 1, RandomY = 2 }
}

byte_enum! {
    /// An environment map's projection.
    Envmap { LatLong = 0, Cube = 1 }
}

byte_enum! {
    /// A tiled part's levels.
    LevelMode { One = 0, Mipmap = 1, Ripmap = 2 }
}

byte_enum! {
    /// How a tiled part's level sizes are rounded.
    RoundingMode { Down = 0, Up = 1 }
}

byte_enum! {
    /// A deep part's state (`deepImageState`).
    DeepImageState { Messy = 0, Sorted = 1, NonOverlapping = 2, Tidy = 3 }
}

/// A part's storage: what Core's `exr_get_storage` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Storage {
    Scanline,
    Tiled,
    DeepScanline,
    DeepTiled,
    /// A part type Core does not know.
    Unknown,
}

impl Storage {
    pub(crate) fn from_core(s: openexr_core::openexr_attr::Storage) -> Storage {
        match s.0 {
            0 => Storage::Scanline,
            1 => Storage::Tiled,
            2 => Storage::DeepScanline,
            3 => Storage::DeepTiled,
            _ => Storage::Unknown,
        }
    }

    /// Whether the part is deep.
    pub fn is_deep(self) -> bool {
        matches!(self, Storage::DeepScanline | Storage::DeepTiled)
    }

    /// Whether the part is tiled.
    pub fn is_tiled(self) -> bool {
        matches!(self, Storage::Tiled | Storage::DeepTiled)
    }
}

/// A channel of a part's channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// The name, as the file holds it (usually, not always, UTF-8).
    pub name: Vec<u8>,
    pub pixel_type: PixelType,
    /// `pLinear`: whether the channel's values are perceptually linear (the file's byte).
    pub p_linear: u8,
    /// The three reserved bytes, as the file holds them.
    pub reserved: [u8; 3],
    pub x_sampling: i32,
    pub y_sampling: i32,
}

impl Channel {
    /// The name, if it is UTF-8.
    pub fn name_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.name).ok()
    }
}

/// A tiled part's tiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileDescription {
    pub x_size: u32,
    pub y_size: u32,
    pub level_mode: LevelMode,
    pub rounding_mode: RoundingMode,
}

/// The CIE xy chromaticities of a part's primaries and white point.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Chromaticities {
    pub red: V2f,
    pub green: V2f,
    pub blue: V2f,
    pub white: V2f,
}

/// A film key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyCode {
    pub film_mfc_code: i32,
    pub film_type: i32,
    pub prefix: i32,
    pub count: i32,
    pub perf_offset: i32,
    pub perfs_per_frame: i32,
    pub perfs_per_count: i32,
}

/// An SMPTE time code, as the file holds it: the time and flags, and the user data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TimeCode {
    pub time_and_flags: u32,
    pub user_data: u32,
}

/// Port of `bitField` (src/lib/OpenEXR/ImfTimeCode.cpp:103-109 @ v3.5.3), for fields narrower
/// than 32 bits.
fn bit_field(value: u32, min_bit: u32, max_bit: u32) -> u32 {
    let mask = !(!0u32 << (max_bit - min_bit + 1)) << min_bit;
    (value & mask) >> min_bit
}

/// Port of `bcdToBinary` (src/lib/OpenEXR/ImfTimeCode.cpp:119-123 @ v3.5.3).
fn bcd_to_binary(bcd: u32) -> u32 {
    (bcd & 0x0f) + 10 * ((bcd >> 4) & 0x0f)
}

impl TimeCode {
    /// Port of `TimeCode::hours` (src/lib/OpenEXR/ImfTimeCode.cpp:135-139 @ v3.5.3), the time
    /// packed as TV60 (the file's own packing).
    pub fn hours(&self) -> u32 {
        bcd_to_binary(bit_field(self.time_and_flags, 24, 29))
    }
    /// Port of `TimeCode::minutes` (src/lib/OpenEXR/ImfTimeCode.cpp:151-155 @ v3.5.3).
    pub fn minutes(&self) -> u32 {
        bcd_to_binary(bit_field(self.time_and_flags, 16, 22))
    }
    /// Port of `TimeCode::seconds` (src/lib/OpenEXR/ImfTimeCode.cpp:167-171 @ v3.5.3).
    pub fn seconds(&self) -> u32 {
        bcd_to_binary(bit_field(self.time_and_flags, 8, 14))
    }
    /// Port of `TimeCode::frame` (src/lib/OpenEXR/ImfTimeCode.cpp:183-187 @ v3.5.3).
    pub fn frame(&self) -> u32 {
        bcd_to_binary(bit_field(self.time_and_flags, 0, 5))
    }
    /// Port of `TimeCode::dropFrame` (src/lib/OpenEXR/ImfTimeCode.cpp:199-203 @ v3.5.3).
    pub fn drop_frame(&self) -> bool {
        bit_field(self.time_and_flags, 6, 6) != 0
    }
    /// Port of `TimeCode::colorFrame` (src/lib/OpenEXR/ImfTimeCode.cpp:211-215 @ v3.5.3).
    pub fn color_frame(&self) -> bool {
        bit_field(self.time_and_flags, 7, 7) != 0
    }
    /// Port of `TimeCode::fieldPhase` (src/lib/OpenEXR/ImfTimeCode.cpp:223-227 @ v3.5.3).
    pub fn field_phase(&self) -> bool {
        bit_field(self.time_and_flags, 15, 15) != 0
    }
    /// Port of `TimeCode::bgf0` (src/lib/OpenEXR/ImfTimeCode.cpp:235-239 @ v3.5.3).
    pub fn bgf0(&self) -> bool {
        bit_field(self.time_and_flags, 23, 23) != 0
    }
    /// Port of `TimeCode::bgf1` (src/lib/OpenEXR/ImfTimeCode.cpp:247-251 @ v3.5.3).
    pub fn bgf1(&self) -> bool {
        bit_field(self.time_and_flags, 30, 30) != 0
    }
    /// Port of `TimeCode::bgf2` (src/lib/OpenEXR/ImfTimeCode.cpp:259-263 @ v3.5.3).
    pub fn bgf2(&self) -> bool {
        bit_field(self.time_and_flags, 31, 31) != 0
    }
}

/// A rational number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rational {
    pub num: i32,
    pub denom: u32,
}

/// A preview image: 8-bit RGBA pixels, row by row.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An attribute's value.
#[derive(Debug, Clone, PartialEq)]
pub enum Attribute {
    Box2i(Box2i),
    Box2f(Box2f),
    /// `bytes`: the type hint and the data.
    Bytes {
        type_hint: Vec<u8>,
        data: Vec<u8>,
    },
    Channels(Vec<Channel>),
    Chromaticities(Chromaticities),
    Compression(Compression),
    Double(f64),
    Envmap(Envmap),
    Float(f32),
    FloatVector(Vec<f32>),
    Int(i32),
    KeyCode(KeyCode),
    LineOrder(LineOrder),
    M33f([f32; 9]),
    M33d([f64; 9]),
    M44f([f32; 16]),
    M44d([f64; 16]),
    Preview(Preview),
    Rational(Rational),
    /// A string, its bytes as the file holds them.
    String(Vec<u8>),
    StringVector(Vec<Vec<u8>>),
    Tiles(TileDescription),
    TimeCode(TimeCode),
    V2i(V2i),
    V2f(V2f),
    V2d(V2d),
    V3i(V3i),
    V3f(V3f),
    V3d(V3d),
    DeepImageState(DeepImageState),
    /// A type Core does not know: its type name and its bytes, as the file holds them.
    Opaque {
        type_name: Vec<u8>,
        data: Vec<u8>,
    },
}

impl Attribute {
    /// The attribute's value as Core holds it (`type_name` is the file's type name, which an
    /// opaque attribute keeps).
    pub(crate) fn from_core(v: &AttrValue, type_name: &[u8]) -> Attribute {
        let s = |s: &openexr_core::string::AttrString| s.as_bytes().to_vec();
        match v {
            AttrValue::Box2i(b) => Attribute::Box2i(Box2 {
                min: V2 {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: V2 {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
            AttrValue::Box2f(b) => Attribute::Box2f(Box2 {
                min: V2 {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: V2 {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
            AttrValue::Bytes(b) => Attribute::Bytes {
                // the `hint_length` bytes, without the NUL Core adds
                type_hint: b
                    .type_hint()
                    .map_or(&[][..], |h| h.get(..b.hint_length() as usize).unwrap_or(h))
                    .to_vec(),
                data: b.data().unwrap_or(&[]).to_vec(),
            },
            AttrValue::Chlist(c) => Attribute::Channels(
                c.entries()
                    .iter()
                    .map(|e| Channel {
                        name: s(&e.name),
                        pixel_type: PixelType::from_core(e.pixel_type),
                        p_linear: e.p_linear,
                        reserved: e.reserved,
                        x_sampling: e.x_sampling,
                        y_sampling: e.y_sampling,
                    })
                    .collect(),
            ),
            AttrValue::Chromaticities(c) => Attribute::Chromaticities(Chromaticities {
                red: V2 {
                    x: c.red_x,
                    y: c.red_y,
                },
                green: V2 {
                    x: c.green_x,
                    y: c.green_y,
                },
                blue: V2 {
                    x: c.blue_x,
                    y: c.blue_y,
                },
                white: V2 {
                    x: c.white_x,
                    y: c.white_y,
                },
            }),
            AttrValue::Compression(v) => Attribute::Compression(Compression::from_value(*v)),
            AttrValue::Double(v) => Attribute::Double(*v),
            AttrValue::Envmap(v) => Attribute::Envmap(Envmap::from_value(*v)),
            AttrValue::Float(v) => Attribute::Float(*v),
            AttrValue::FloatVector(v) => Attribute::FloatVector(v.arr().to_vec()),
            AttrValue::Int(v) => Attribute::Int(*v),
            AttrValue::Keycode(k) => Attribute::KeyCode(KeyCode {
                film_mfc_code: k.film_mfc_code,
                film_type: k.film_type,
                prefix: k.prefix,
                count: k.count,
                perf_offset: k.perf_offset,
                perfs_per_frame: k.perfs_per_frame,
                perfs_per_count: k.perfs_per_count,
            }),
            AttrValue::LineOrder(v) => Attribute::LineOrder(LineOrder::from_value(*v)),
            AttrValue::M33f(m) => Attribute::M33f(m.m),
            AttrValue::M33d(m) => Attribute::M33d(m.m),
            AttrValue::M44f(m) => Attribute::M44f(m.m),
            AttrValue::M44d(m) => Attribute::M44d(m.m),
            AttrValue::Preview(p) => Attribute::Preview(Preview {
                width: p.width(),
                height: p.height(),
                rgba: p.rgba().unwrap_or(&[]).to_vec(),
            }),
            AttrValue::Rational(r) => Attribute::Rational(Rational {
                num: r.num,
                denom: r.denom,
            }),
            AttrValue::String(v) => Attribute::String(s(v)),
            AttrValue::StringVector(v) => {
                Attribute::StringVector(v.strings().iter().map(s).collect())
            }
            AttrValue::TileDesc(t) => Attribute::Tiles(TileDescription {
                x_size: t.x_size,
                y_size: t.y_size,
                level_mode: LevelMode::from_value(t.level_and_round & 0xF),
                rounding_mode: RoundingMode::from_value(t.level_and_round >> 4),
            }),
            AttrValue::Timecode(t) => Attribute::TimeCode(TimeCode {
                time_and_flags: t.time_and_flags,
                user_data: t.user_data,
            }),
            AttrValue::V2i(v) => Attribute::V2i(V2 { x: v.x, y: v.y }),
            AttrValue::V2f(v) => Attribute::V2f(V2 { x: v.x, y: v.y }),
            AttrValue::V2d(v) => Attribute::V2d(V2 { x: v.x, y: v.y }),
            AttrValue::V3i(v) => Attribute::V3i(V3 {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
            AttrValue::V3f(v) => Attribute::V3f(V3 {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
            AttrValue::V3d(v) => Attribute::V3d(V3 {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
            AttrValue::DeepImageState(v) => {
                Attribute::DeepImageState(DeepImageState::from_value(*v))
            }
            AttrValue::Opaque(o) => Attribute::Opaque {
                type_name: type_name.to_vec(),
                data: o.packed_data().unwrap_or(&[]).to_vec(),
            },
        }
    }

    /// The type's name as the file spells it (`box2i`, `chlist`, ...; an opaque attribute's
    /// own).
    pub fn type_name(&self) -> &[u8] {
        match self {
            Attribute::Box2i(_) => b"box2i",
            Attribute::Box2f(_) => b"box2f",
            Attribute::Bytes { .. } => b"bytes",
            Attribute::Channels(_) => b"chlist",
            Attribute::Chromaticities(_) => b"chromaticities",
            Attribute::Compression(_) => b"compression",
            Attribute::Double(_) => b"double",
            Attribute::Envmap(_) => b"envmap",
            Attribute::Float(_) => b"float",
            Attribute::FloatVector(_) => b"floatvector",
            Attribute::Int(_) => b"int",
            Attribute::KeyCode(_) => b"keycode",
            Attribute::LineOrder(_) => b"lineOrder",
            Attribute::M33f(_) => b"m33f",
            Attribute::M33d(_) => b"m33d",
            Attribute::M44f(_) => b"m44f",
            Attribute::M44d(_) => b"m44d",
            Attribute::Preview(_) => b"preview",
            Attribute::Rational(_) => b"rational",
            Attribute::String(_) => b"string",
            Attribute::StringVector(_) => b"stringvector",
            Attribute::Tiles(_) => b"tiledesc",
            Attribute::TimeCode(_) => b"timecode",
            Attribute::V2i(_) => b"v2i",
            Attribute::V2f(_) => b"v2f",
            Attribute::V2d(_) => b"v2d",
            Attribute::V3i(_) => b"v3i",
            Attribute::V3f(_) => b"v3f",
            Attribute::V3d(_) => b"v3d",
            Attribute::DeepImageState(_) => b"deepImageState",
            Attribute::Opaque { type_name, .. } => type_name,
        }
    }
}
