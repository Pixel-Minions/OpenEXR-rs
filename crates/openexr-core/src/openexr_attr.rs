// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Port of `openexr_attr.h` (src/lib/OpenEXRCore/openexr_attr.h:1-523 @ v3.5.3): the enums
//! and the fixed-size attribute structs. The allocating types (strings, vectors, channel
//! lists, ...) live with their functions, as upstream's do.
//!
//! Upstream's enums are C `enum`s, and files and callers can hold values outside them (a
//! header's compression byte, an `int` passed by a caller), which validation then rejects.
//! So each is a transparent newtype over its C integer, with the enumerators as associated
//! constants, and holds any value C can.
//!
//! The fixed-size structs are `#[repr(C)]`, `tiledesc` packed as upstream packs it
//! (`#pragma pack(push, 1)`, lines 125-262), so their sizes match the C structs
//! (`testAttrSizes`).

macro_rules! c_enum {
    ($(#[$doc:meta])* $name:ident: $repr:ty { $($(#[$vdoc:meta])* $var:ident = $val:expr),* $(,)? }) => {
        $(#[$doc])*
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub $repr);

        impl $name {
            $($(#[$vdoc])* pub const $var: $name = $name($val);)*
        }
    };
}

c_enum! {
    /// Port of `exr_compression_t` (src/lib/OpenEXRCore/openexr_attr.h:36-53 @ v3.5.3).
    Compression: i32 {
        NONE = 0, RLE = 1, ZIPS = 2, ZIP = 3, PIZ = 4, PXR24 = 5, B44 = 6, B44A = 7, DWAA = 8,
        DWAB = 9, HTJ2K256 = 10, HTJ2K32 = 11, LJ2K = 12, ZSTD = 13,
        /// Invalid value, provided for range checking.
        LAST_TYPE = 14,
    }
}

c_enum! {
    /// Port of `exr_envmap_t` (src/lib/OpenEXRCore/openexr_attr.h:56-61 @ v3.5.3).
    Envmap: i32 { LATLONG = 0, CUBE = 1, LAST_TYPE = 2 }
}

c_enum! {
    /// Port of `exr_lineorder_t` (src/lib/OpenEXRCore/openexr_attr.h:64-70 @ v3.5.3).
    LineOrder: i32 { INCREASING_Y = 0, DECREASING_Y = 1, RANDOM_Y = 2, LAST_TYPE = 3 }
}

c_enum! {
    /// Port of `exr_storage_t` (src/lib/OpenEXRCore/openexr_attr.h:73-81 @ v3.5.3).
    Storage: i32 {
        SCANLINE = 0, TILED = 1, DEEP_SCANLINE = 2, DEEP_TILED = 3,
        /// Invalid value, provided for range checking.
        LAST_TYPE = 4,
        /// An unknown type, provided for future proofing.
        UNKNOWN = 5,
    }
}

c_enum! {
    /// Port of `exr_tile_level_mode_t` (src/lib/OpenEXRCore/openexr_attr.h:84-90 @ v3.5.3).
    TileLevelMode: i32 { ONE_LEVEL = 0, MIPMAP_LEVELS = 1, RIPMAP_LEVELS = 2, LAST_TYPE = 3 }
}

c_enum! {
    /// Port of `exr_tile_round_mode_t` (src/lib/OpenEXRCore/openexr_attr.h:93-98 @ v3.5.3).
    TileRoundMode: i32 { ROUND_DOWN = 0, ROUND_UP = 1, LAST_TYPE = 2 }
}

c_enum! {
    /// Port of `exr_pixel_type_t` (src/lib/OpenEXRCore/openexr_attr.h:101-107 @ v3.5.3).
    PixelType: i32 { UINT = 0, HALF = 1, FLOAT = 2, LAST_TYPE = 3 }
}

c_enum! {
    /// Port of `exr_deep_image_state_t` (src/lib/OpenEXRCore/openexr_attr.h:110-117 @ v3.5.3).
    DeepImageState: i32 { MESSY = 0, SORTED = 1, NON_OVERLAPPING = 2, TIDY = 3, LAST_TYPE = 4 }
}

c_enum! {
    /// Port of `exr_perceptual_treatment_t` (src/lib/OpenEXRCore/openexr_attr.h:303-307 @ v3.5.3).
    PerceptualTreatment: i32 { LOGARITHMIC = 0, LINEAR = 1 }
}

c_enum! {
    /// Port of `exr_attribute_type_t` (src/lib/OpenEXRCore/openexr_attr.h:405-441 @ v3.5.3).
    AttributeType: i32 {
        /// Type indicating an error or uninitialized attribute.
        UNKNOWN = 0,
        BOX2I = 1, BOX2F = 2, BYTES = 3, CHLIST = 4, CHROMATICITIES = 5, COMPRESSION = 6,
        DOUBLE = 7, ENVMAP = 8, FLOAT = 9, FLOAT_VECTOR = 10, INT = 11, KEYCODE = 12,
        LINEORDER = 13, M33F = 14, M33D = 15, M44F = 16, M44D = 17, PREVIEW = 18,
        RATIONAL = 19, STRING = 20, STRING_VECTOR = 21, TILEDESC = 22, TIMECODE = 23,
        V2I = 24, V2F = 25, V2D = 26, V3I = 27, V3F = 28, V3D = 29, DEEP_IMAGE_STATE = 30,
        /// User/unknown provided type.
        OPAQUE = 31,
        LAST_KNOWN_TYPE = 32,
    }
}

/// Port of `exr_attr_chromaticities_t` (src/lib/OpenEXRCore/openexr_attr.h:128-138 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Chromaticities {
    pub red_x: f32,
    pub red_y: f32,
    pub green_x: f32,
    pub green_y: f32,
    pub blue_x: f32,
    pub blue_y: f32,
    pub white_x: f32,
    pub white_y: f32,
}

/// Port of `exr_attr_keycode_t` (src/lib/OpenEXRCore/openexr_attr.h:141-150 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Keycode {
    pub film_mfc_code: i32,
    pub film_type: i32,
    pub prefix: i32,
    pub count: i32,
    pub perf_offset: i32,
    pub perfs_per_frame: i32,
    pub perfs_per_count: i32,
}

/// Port of `exr_attr_m33f_t` (src/lib/OpenEXRCore/openexr_attr.h:153-156 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct M33f {
    pub m: [f32; 9],
}

/// Port of `exr_attr_m33d_t` (src/lib/OpenEXRCore/openexr_attr.h:159-162 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct M33d {
    pub m: [f64; 9],
}

/// Port of `exr_attr_m44f_t` (src/lib/OpenEXRCore/openexr_attr.h:165-168 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct M44f {
    pub m: [f32; 16],
}

/// Port of `exr_attr_m44d_t` (src/lib/OpenEXRCore/openexr_attr.h:171-174 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct M44d {
    pub m: [f64; 16],
}

/// Port of `exr_attr_rational_t` (src/lib/OpenEXRCore/openexr_attr.h:177-181 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rational {
    pub num: i32,
    pub denom: u32,
}

/// Port of `exr_attr_timecode_t` (src/lib/OpenEXRCore/openexr_attr.h:184-188 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timecode {
    pub time_and_flags: u32,
    pub user_data: u32,
}

/// Port of `exr_attr_v2i_t` (src/lib/OpenEXRCore/openexr_attr.h:191-194 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct V2i {
    pub x: i32,
    pub y: i32,
}

/// Port of `exr_attr_v2f_t` (src/lib/OpenEXRCore/openexr_attr.h:197-200 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V2f {
    pub x: f32,
    pub y: f32,
}

/// Port of `exr_attr_v2d_t` (src/lib/OpenEXRCore/openexr_attr.h:203-206 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V2d {
    pub x: f64,
    pub y: f64,
}

/// Port of `exr_attr_v3i_t` (src/lib/OpenEXRCore/openexr_attr.h:209-212 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct V3i {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

/// Port of `exr_attr_v3f_t` (src/lib/OpenEXRCore/openexr_attr.h:215-218 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V3f {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// Port of `exr_attr_v3d_t` (src/lib/OpenEXRCore/openexr_attr.h:221-224 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct V3d {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

/// Port of `exr_attr_box2i_t` (src/lib/OpenEXRCore/openexr_attr.h:227-231 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Box2i {
    pub min: V2i,
    pub max: V2i,
}

/// Port of `exr_attr_box2f_t` (src/lib/OpenEXRCore/openexr_attr.h:234-238 @ v3.5.3).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Box2f {
    pub min: V2f,
    pub max: V2f,
}

/// Port of `exr_attr_tiledesc_t` (src/lib/OpenEXRCore/openexr_attr.h:245-250 @ v3.5.3), packed.
#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TileDesc {
    pub x_size: u32,
    pub y_size: u32,
    pub level_and_round: u8,
}

impl TileDesc {
    /// Port of `EXR_GET_TILE_LEVEL_MODE` (src/lib/OpenEXRCore/openexr_attr.h:253-254 @ v3.5.3).
    pub fn level_mode(&self) -> TileLevelMode {
        TileLevelMode(i32::from(self.level_and_round & 0xF))
    }

    /// Port of `EXR_GET_TILE_ROUND_MODE` (src/lib/OpenEXRCore/openexr_attr.h:256-257 @ v3.5.3).
    pub fn round_mode(&self) -> TileRoundMode {
        TileRoundMode(i32::from((self.level_and_round >> 4) & 0xF))
    }
}

/// Port of `EXR_PACK_TILE_LEVEL_ROUND` (src/lib/OpenEXRCore/openexr_attr.h:259-260 @ v3.5.3).
pub fn pack_tile_level_round(lvl: TileLevelMode, mode: TileRoundMode) -> u8 {
    (((mode.0 & 0xF) as u8) << 4) | ((lvl.0 & 0xF) as u8)
}
