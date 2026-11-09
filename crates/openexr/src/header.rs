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

    /// A header for a scanline part whose pixels are `data_window`, with the C++ library's
    /// defaults (`initialize`, src/lib/OpenEXR/ImfHeader.cpp:206-226 @ v3.5.3): the display
    /// window the data window, pixel aspect ratio 1, screen window centre (0, 0) and width 1,
    /// increasing Y, ZIP compression, no channels.
    pub fn new(data_window: Box2i) -> Header {
        let mut h = Header {
            storage: Storage::Scanline,
            attributes: Vec::new(),
        };
        h.insert("displayWindow", Attribute::Box2i(data_window));
        h.insert("dataWindow", Attribute::Box2i(data_window));
        h.insert("pixelAspectRatio", Attribute::Float(1.0));
        h.insert("screenWindowCenter", Attribute::V2f(V2 { x: 0.0, y: 0.0 }));
        h.insert("screenWindowWidth", Attribute::Float(1.0));
        h.insert("lineOrder", Attribute::LineOrder(LineOrder::IncreasingY));
        h.insert("compression", Attribute::Compression(Compression::Zip));
        h.insert("channels", Attribute::Channels(Vec::new()));
        h
    }

    /// Sets attribute `name`, replacing its value where the header has it (in its place),
    /// else adding it last; returns the value it replaced.
    pub fn insert(&mut self, name: impl AsRef<[u8]>, value: Attribute) -> Option<Attribute> {
        let name = name.as_ref();
        match self.attributes.iter_mut().find(|(n, _)| n == name) {
            Some((_, v)) => Some(std::mem::replace(v, value)),
            None => {
                self.attributes.push((name.to_vec(), value));
                None
            }
        }
    }

    /// Removes attribute `name`; returns its value.
    pub fn remove(&mut self, name: impl AsRef<[u8]>) -> Option<Attribute> {
        let name = name.as_ref();
        let i = self.attributes.iter().position(|(n, _)| n == name)?;
        Some(self.attributes.remove(i).1)
    }

    /// Sets the part's storage. A tiled part needs `tiles` ([`set_tiles`](Self::set_tiles)).
    pub fn set_storage(&mut self, storage: Storage) {
        self.storage = storage;
    }

    /// Adds channel `channel`, or replaces the one of its name, keeping the list sorted by
    /// name as Core keeps it.
    pub fn insert_channel(&mut self, channel: Channel) {
        let mut chans = self.channels().to_vec();
        match chans.iter_mut().find(|c| c.name == channel.name) {
            Some(c) => *c = channel,
            None => chans.push(channel),
        }
        chans.sort_by(|a, b| a.name.cmp(&b.name));
        self.insert("channels", Attribute::Channels(chans));
    }

    /// Sets `compression`.
    pub fn set_compression(&mut self, compression: Compression) {
        self.insert("compression", Attribute::Compression(compression));
    }

    /// Sets `lineOrder`.
    pub fn set_line_order(&mut self, line_order: LineOrder) {
        self.insert("lineOrder", Attribute::LineOrder(line_order));
    }

    /// Sets `tiles`; a scanline part becomes tiled (a deep scanline part, deep tiled).
    pub fn set_tiles(&mut self, tiles: TileDescription) {
        self.insert("tiles", Attribute::Tiles(tiles));
        self.storage = match self.storage {
            Storage::Scanline => Storage::Tiled,
            Storage::DeepScanline => Storage::DeepTiled,
            s => s,
        };
    }

    /// Sets `name`, the part's name.
    pub fn set_name(&mut self, name: impl AsRef<[u8]>) {
        self.insert("name", Attribute::String(name.as_ref().to_vec()));
    }

    /// Sets `dataWindow`.
    pub fn set_data_window(&mut self, window: Box2i) {
        self.insert("dataWindow", Attribute::Box2i(window));
    }

    /// Sets `displayWindow`.
    pub fn set_display_window(&mut self, window: Box2i) {
        self.insert("displayWindow", Attribute::Box2i(window));
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
