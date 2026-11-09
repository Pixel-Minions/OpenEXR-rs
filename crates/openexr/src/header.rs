// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! A part's header: every attribute the file holds, in the file's order, with the standard
//! ones as typed accessors.

use crate::attribute::{
    Attribute, Box2i, Channel, Chromaticities, Compression, LineOrder, Storage, TileDescription,
    V2, V2f,
};

/// A part's header, as Core read it: its storage and its attributes in the file's order (the
/// ones Core adds when a header lacks a required attribute come last, as Core adds them).
///
/// Reading a file, Core checks the required attributes (`channels`, `compression`,
/// `dataWindow`, `displayWindow`, `lineOrder`, `pixelAspectRatio`, `screenWindowCenter`,
/// `screenWindowWidth`, and `tiles` for a tiled part) and, unless the header check is strict,
/// adds the defaults of the missing ones (src/lib/OpenEXRCore/validation.c:22-293 @ v3.5.3); their accessors therefore always
/// have a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    storage: Storage,
    attributes: Vec<(Vec<u8>, Attribute)>,
}

impl Header {
    pub(crate) fn from_parts(storage: Storage, attributes: Vec<(Vec<u8>, Attribute)>) -> Header {
        Header {
            storage,
            attributes,
        }
    }

    /// The part's storage: scanlines or tiles, flat or deep.
    pub fn storage(&self) -> Storage {
        self.storage
    }

    /// The attribute `name`, if the header has it.
    pub fn get(&self, name: impl AsRef<[u8]>) -> Option<&Attribute> {
        let name = name.as_ref();
        self.attributes
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, a)| a)
    }

    /// Every attribute, name and value, in the file's order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&[u8], &Attribute)> {
        self.attributes.iter().map(|(n, a)| (n.as_slice(), a))
    }

    /// The number of attributes.
    pub fn len(&self) -> usize {
        self.attributes.len()
    }

    /// Whether the header has no attributes (never, for a header read from a file).
    pub fn is_empty(&self) -> bool {
        self.attributes.is_empty()
    }

    fn box2i(&self, name: &str) -> Box2i {
        match self.get(name) {
            Some(Attribute::Box2i(b)) => *b,
            // Core's default (src/lib/OpenEXRCore/validation.c:56-70 @ v3.5.3)
            _ => Box2i {
                min: V2 { x: 0, y: 0 },
                max: V2 { x: 63, y: 63 },
            },
        }
    }

    fn float(&self, name: &str) -> f32 {
        match self.get(name) {
            Some(Attribute::Float(v)) => *v,
            // Core's default (src/lib/OpenEXRCore/validation.c:148-161, 207-220 @ v3.5.3)
            _ => 1.0,
        }
    }

    /// `dataWindow`: the pixels the part holds.
    pub fn data_window(&self) -> Box2i {
        self.box2i("dataWindow")
    }

    /// `displayWindow`: the image's frame.
    pub fn display_window(&self) -> Box2i {
        self.box2i("displayWindow")
    }

    /// `channels`, sorted by name, as the file holds them.
    pub fn channels(&self) -> &[Channel] {
        match self.get("channels") {
            Some(Attribute::Channels(c)) => c,
            _ => &[],
        }
    }

    /// The channel `name`, if the part has it.
    pub fn channel(&self, name: impl AsRef<[u8]>) -> Option<&Channel> {
        let name = name.as_ref();
        self.channels().iter().find(|c| c.name == name)
    }

    /// `compression`.
    pub fn compression(&self) -> Compression {
        match self.get("compression") {
            Some(Attribute::Compression(c)) => *c,
            // Core's default (src/lib/OpenEXRCore/validation.c:24-40 @ v3.5.3)
            _ => Compression::Zip,
        }
    }

    /// `lineOrder`.
    pub fn line_order(&self) -> LineOrder {
        match self.get("lineOrder") {
            Some(Attribute::LineOrder(l)) => *l,
            _ => LineOrder::IncreasingY,
        }
    }

    /// `pixelAspectRatio`.
    pub fn pixel_aspect_ratio(&self) -> f32 {
        self.float("pixelAspectRatio")
    }

    /// `screenWindowCenter`.
    pub fn screen_window_center(&self) -> V2f {
        match self.get("screenWindowCenter") {
            Some(Attribute::V2f(v)) => *v,
            _ => V2 { x: 0.0, y: 0.0 },
        }
    }

    /// `screenWindowWidth`.
    pub fn screen_window_width(&self) -> f32 {
        self.float("screenWindowWidth")
    }

    /// `tiles`, for a tiled part.
    pub fn tiles(&self) -> Option<&TileDescription> {
        match self.get("tiles") {
            Some(Attribute::Tiles(t)) => Some(t),
            _ => None,
        }
    }

    fn string(&self, name: &str) -> Option<&[u8]> {
        match self.get(name) {
            Some(Attribute::String(s)) => Some(s),
            _ => None,
        }
    }

    /// `name`: the part's name (required in a multi-part file).
    pub fn name(&self) -> Option<&[u8]> {
        self.string("name")
    }

    /// `type`: the part's type as the file spells it (`scanlineimage`, `tiledimage`,
    /// `deepscanline`, `deeptile`), when the header has one.
    pub fn part_type(&self) -> Option<&[u8]> {
        self.string("type")
    }

    /// `version`, the part's version (deep parts).
    pub fn version(&self) -> Option<i32> {
        match self.get("version") {
            Some(Attribute::Int(v)) => Some(*v),
            _ => None,
        }
    }

    /// `chunkCount`, the part's chunk count (multi-part and deep files).
    pub fn chunk_count(&self) -> Option<i32> {
        match self.get("chunkCount") {
            Some(Attribute::Int(v)) => Some(*v),
            _ => None,
        }
    }

    /// `chromaticities`, when the header has them.
    pub fn chromaticities(&self) -> Option<&Chromaticities> {
        match self.get("chromaticities") {
            Some(Attribute::Chromaticities(c)) => Some(c),
            _ => None,
        }
    }

    /// `dwaCompressionLevel`, when the header has it.
    pub fn dwa_compression_level(&self) -> Option<f32> {
        match self.get("dwaCompressionLevel") {
            Some(Attribute::Float(v)) => Some(*v),
            _ => None,
        }
    }
}
