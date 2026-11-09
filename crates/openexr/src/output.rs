// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Writing a file: the C++ library's `MultiPartOutputFile`, `OutputFile` and `OutputPart`,
//! over Core's writer.
//!
//! The C++ library of 3.5.3 writes with its own code, not Core's, and the two lay a file out
//! alike but for one thing: a single-part file's header. The C++ library writes every
//! attribute the header holds, sorted by name (`Header::writeTo`, src/lib/OpenEXR/
//! ImfHeader.cpp @ v3.5.3); Core, writing a single-part image's header as the C++ library
//! did before multi-part files, sorts them too but leaves out `type` and `name`
//! (src/lib/OpenEXRCore/write_header.c:714-728 @ v3.5.3). So the writer here has Core write
//! the file through a sink of its own, which puts `type` and `name` back into such a header
//! where they sort, and moves everything after it, the chunk offsets included, by their
//! length: the file the C++ library writes, byte for byte.

use std::io::{Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use openexr_core::Context;
use openexr_core::bytes::AttrBytes;
use openexr_core::cfmt::Arg;
use openexr_core::coding::BufferRef;
use openexr_core::context::{
    DefaultWriteMode, EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER, Initializer, StreamErrors,
};
use openexr_core::openexr_attr as ca;
use openexr_core::preview::AttrPreview;

use crate::attribute::{Attribute, LineOrder, Storage};
use crate::error::{Error, ErrorCode, Result};
use crate::frame_buffer::{FrameBuffer, first_multiple, sample_count};
use crate::header::Header;
use crate::read::check_slice;

fn core(e: openexr_core::Error) -> Error {
    Error::from_core(&e)
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, msg)
}

/// How a file is written: Core's compression settings.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WriteOptions {
    zip_level: Option<i32>,
    zstd_level: Option<i32>,
    dwa_quality: Option<f32>,
}

impl WriteOptions {
    /// The defaults: Core's levels, which the C++ library's are.
    pub fn new() -> WriteOptions {
        WriteOptions::default()
    }

    /// The deflate level of ZIP and ZIPS parts (`zip_level`).
    pub fn zip_level(mut self, level: i32) -> WriteOptions {
        self.zip_level = Some(level);
        self
    }

    /// The level of ZSTD parts (`zstd_level`).
    pub fn zstd_level(mut self, level: i32) -> WriteOptions {
        self.zstd_level = Some(level);
        self
    }

    /// The quality of DWAA and DWAB parts without a `dwaCompressionLevel` (`dwa_quality`).
    pub fn dwa_quality(mut self, quality: f32) -> WriteOptions {
        self.dwa_quality = Some(quality);
        self
    }

    /// Creates the file at `path` for `headers`, one part each.
    pub fn create(
        &self,
        path: impl AsRef<Path>,
        headers: &[Header],
    ) -> Result<MultiPartOutputFile<std::fs::File>> {
        let path = path.as_ref();
        let file = std::fs::File::create(path).map_err(|e| {
            Error::new(
                ErrorCode::FileAccess,
                format!("Unable to open file for write: {e}"),
            )
        })?;
        self.create_writer(&path.to_string_lossy(), file, headers)
    }

    /// Writes a file for `headers` to `writer`; `name` is the file name Core's messages give.
    pub fn create_writer<W: Write + Seek + Send + 'static>(
        &self,
        name: &str,
        writer: W,
        headers: &[Header],
    ) -> Result<MultiPartOutputFile<W>> {
        MultiPartOutputFile::start(name, writer, headers, self)
    }
}

/// Where Core's writes go, and how the header is mended on the way.
struct Sink<W> {
    w: W,
    state: State,
}

enum State {
    /// Core is writing the header: its bytes, kept.
    Header(Vec<u8>),
    /// The header is written: every later write lands `delta` bytes on, and the chunk table
    /// of `table` (offset, length) has its offsets moved by as much.
    Body { delta: u64, table: (u64, usize) },
}

impl<W: Write + Seek> Sink<W> {
    fn write(&mut self, errs: &StreamErrors<'_>, buf: &[u8], off: u64) -> i64 {
        let r = match &mut self.state {
            State::Header(h) => {
                let end = off as usize + buf.len();
                if h.len() < end {
                    h.resize(end, 0);
                }
                h[off as usize..end].copy_from_slice(buf);
                Ok(())
            }
            State::Body { delta, table } => {
                let delta = *delta;
                let moved;
                let data = if delta != 0 && (off, buf.len()) == *table {
                    moved = buf
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .flat_map(|e| {
                            let v = u64::from_le_bytes(*e);
                            (if v == 0 { 0 } else { v + delta }).to_le_bytes()
                        })
                        .collect::<Vec<u8>>();
                    &moved[..]
                } else {
                    buf
                };
                self.w
                    .seek(SeekFrom::Start(off + delta))
                    .and_then(|_| self.w.write_all(data))
            }
        };
        match r {
            Ok(()) => buf.len() as i64,
            Err(e) => {
                let msg = e.to_string();
                errs.print_error(
                    ErrorCode::WriteIo,
                    "Unable to write %llu bytes: %s",
                    &[Arg::U64(buf.len() as u64), Arg::Str(msg.as_bytes())],
                );
                -1
            }
        }
    }
}

/// The C++ library's single-part header from Core's: `inserts`, string attributes, added
/// to the records Core wrote, all sorted by name as `Header::writeTo` writes a
/// `std::map<Name, Attribute*>` (`strcmp` order).
fn cpp_header(core: &[u8], inserts: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut records: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    let mut at = 8;
    while at < core.len() && core[at] != 0 {
        let start = at;
        let name_end = at + core[at..].iter().position(|&b| b == 0).unwrap_or(0);
        let type_end = name_end
            + 1
            + core[name_end + 1..]
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(0);
        let size = i32::from_le_bytes(
            core[type_end + 1..type_end + 5]
                .try_into()
                .expect("4 bytes"),
        );
        at = type_end + 5 + size.max(0) as usize;
        records.push((core[start..name_end].to_vec(), core[start..at].to_vec()));
    }
    for (name, value) in inserts {
        let mut r = name.to_vec();
        r.push(0);
        r.extend_from_slice(b"string\0");
        r.extend_from_slice(&(value.len() as i32).to_le_bytes());
        r.extend_from_slice(value);
        records.push((name.to_vec(), r));
    }
    records.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = core[..8].to_vec();
    for (_, r) in records {
        out.extend_from_slice(&r);
    }
    out.push(0);
    out
}

fn utf8<'s>(b: &'s [u8], what: &str) -> Result<&'s str> {
    std::str::from_utf8(b)
        .map_err(|_| invalid(format!("{what} is not UTF-8, which Core needs to write it")))
}

/// Sets attribute `name` of part `p` to `a` through Core's setters.
fn set_attribute(ctx: &mut Context, p: i32, name: &str, a: &Attribute) -> Result<()> {
    let n = Some(name);
    let r = match a {
        Attribute::Box2i(b) => ctx.attr_set_box2i(
            p,
            n,
            Some(&ca::Box2i {
                min: ca::V2i {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: ca::V2i {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
        ),
        Attribute::Box2f(b) => ctx.attr_set_box2f(
            p,
            n,
            Some(&ca::Box2f {
                min: ca::V2f {
                    x: b.min.x,
                    y: b.min.y,
                },
                max: ca::V2f {
                    x: b.max.x,
                    y: b.max.y,
                },
            }),
        ),
        Attribute::Bytes { type_hint, data } => {
            ctx.attr_set_bytes(p, n, Some(&AttrBytes::borrowed(type_hint, data)))
        }
        Attribute::Channels(chans) => {
            if name != "channels" {
                return Err(invalid(format!(
                    "Attribute \"{name}\": a channel list other than \"channels\" cannot be written"
                )));
            }
            for c in chans {
                let cname = utf8(&c.name, "A channel name")?;
                ctx.add_channel(
                    p,
                    Some(cname),
                    ca::PixelType(c.pixel_type.value()),
                    ca::PerceptualTreatment(i32::from(c.p_linear)),
                    c.x_sampling,
                    c.y_sampling,
                )
                .map_err(core)?;
            }
            Ok(())
        }
        Attribute::Chromaticities(c) => ctx.attr_set_chromaticities(
            p,
            n,
            Some(&ca::Chromaticities {
                red_x: c.red.x,
                red_y: c.red.y,
                green_x: c.green.x,
                green_y: c.green.y,
                blue_x: c.blue.x,
                blue_y: c.blue.y,
                white_x: c.white.x,
                white_y: c.white.y,
            }),
        ),
        Attribute::Compression(c) => {
            ctx.attr_set_compression(p, n, ca::Compression(i32::from(c.value())))
        }
        Attribute::Double(v) => ctx.attr_set_double(p, n, *v),
        Attribute::Envmap(e) => ctx.attr_set_envmap(p, n, ca::Envmap(i32::from(e.value()))),
        Attribute::Float(v) => ctx.attr_set_float(p, n, *v),
        Attribute::FloatVector(v) => ctx.attr_set_float_vector(p, n, v.len() as i32, Some(v)),
        Attribute::Int(v) => ctx.attr_set_int(p, n, *v),
        Attribute::KeyCode(k) => ctx.attr_set_keycode(
            p,
            n,
            Some(&ca::Keycode {
                film_mfc_code: k.film_mfc_code,
                film_type: k.film_type,
                prefix: k.prefix,
                count: k.count,
                perf_offset: k.perf_offset,
                perfs_per_frame: k.perfs_per_frame,
                perfs_per_count: k.perfs_per_count,
            }),
        ),
        Attribute::LineOrder(l) => {
            ctx.attr_set_lineorder(p, n, ca::LineOrder(i32::from(l.value())))
        }
        Attribute::M33f(m) => ctx.attr_set_m33f(p, n, Some(&ca::M33f { m: *m })),
        Attribute::M33d(m) => ctx.attr_set_m33d(p, n, Some(&ca::M33d { m: *m })),
        Attribute::M44f(m) => ctx.attr_set_m44f(p, n, Some(&ca::M44f { m: *m })),
        Attribute::M44d(m) => ctx.attr_set_m44d(p, n, Some(&ca::M44d { m: *m })),
        Attribute::Preview(pv) => ctx.attr_set_preview(
            p,
            n,
            Some(&AttrPreview::borrowed(pv.width, pv.height, Some(&pv.rgba))),
        ),
        Attribute::Rational(r) => ctx.attr_set_rational(
            p,
            n,
            Some(&ca::Rational {
                num: r.num,
                denom: r.denom,
            }),
        ),
        Attribute::String(s) => ctx.attr_set_string(p, n, Some(utf8(s, "A string attribute")?)),
        Attribute::StringVector(v) => {
            let strs = v
                .iter()
                .map(|s| utf8(s, "A string vector attribute").map(Some))
                .collect::<Result<Vec<Option<&str>>>>()?;
            ctx.attr_set_string_vector(p, n, strs.len() as i32, Some(&strs))
        }
        Attribute::Tiles(t) => ctx.attr_set_tiledesc(
            p,
            n,
            Some(&ca::TileDesc {
                x_size: t.x_size,
                y_size: t.y_size,
                level_and_round: (t.level_mode.value() & 0xF) | (t.rounding_mode.value() << 4),
            }),
        ),
        Attribute::TimeCode(t) => ctx.attr_set_timecode(
            p,
            n,
            Some(&ca::Timecode {
                time_and_flags: t.time_and_flags,
                user_data: t.user_data,
            }),
        ),
        Attribute::V2i(v) => ctx.attr_set_v2i(p, n, Some(&ca::V2i { x: v.x, y: v.y })),
        Attribute::V2f(v) => ctx.attr_set_v2f(p, n, Some(&ca::V2f { x: v.x, y: v.y })),
        Attribute::V2d(v) => ctx.attr_set_v2d(p, n, Some(&ca::V2d { x: v.x, y: v.y })),
        Attribute::V3i(v) => ctx.attr_set_v3i(
            p,
            n,
            Some(&ca::V3i {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::V3f(v) => ctx.attr_set_v3f(
            p,
            n,
            Some(&ca::V3f {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::V3d(v) => ctx.attr_set_v3d(
            p,
            n,
            Some(&ca::V3d {
                x: v.x,
                y: v.y,
                z: v.z,
            }),
        ),
        Attribute::DeepImageState(s) => {
            let id = ctx
                .attr_declare(p, n, ca::AttributeType::DEEP_IMAGE_STATE)
                .map_err(core)?;
            *ctx.declared_value_mut(p, id).map_err(core)? =
                openexr_core::attributes::AttrValue::DeepImageState(s.value());
            Ok(())
        }
        Attribute::Opaque { type_name, data } => ctx.attr_set_user(
            p,
            n,
            Some(utf8(type_name, "An attribute type name")?),
            data.len() as i32,
            None,
            Some(data),
        ),
    };
    r.map_err(core)
}

fn core_storage(s: Storage) -> Result<ca::Storage> {
    Ok(match s {
        Storage::Scanline => ca::Storage::SCANLINE,
        Storage::Tiled => ca::Storage::TILED,
        Storage::DeepScanline => ca::Storage::DEEP_SCANLINE,
        Storage::DeepTiled => ca::Storage::DEEP_TILED,
        Storage::Unknown => return Err(invalid("A part of unknown type cannot be written")),
    })
}

fn type_string(s: Storage) -> &'static [u8] {
    match s {
        Storage::Scanline => b"scanlineimage",
        Storage::Tiled => b"tiledimage",
        Storage::DeepScanline => b"deepscanline",
        _ => b"deeptile",
    }
}

/// A chunk of lines on its way to the file: each channel's samples, in the file's type, as
/// the C++ library's line buffer holds them until the chunk is whole.
struct Staged {
    first: i32,
    last: i32,
    lines: i32,
    planes: Vec<Vec<u8>>,
}

/// Encodes one chunk, `winfo`, of part `p` from `planes`, one per channel of `h` in the
/// file's type, through Core's encode pipeline with its default stages, which writes it.
fn encode_chunk(
    ctx: &mut Context,
    p: i32,
    winfo: &openexr_core::chunk_info::ChunkInfo,
    h: &Header,
    planes: &[Vec<u8>],
) -> Result<()> {
    let mut enc = ctx.encoding_initialize(p, winfo).map_err(core)?;
    for ch in enc.channels.iter_mut() {
        let k = h
            .channels()
            .iter()
            .position(|c| c.name == ch.channel_name)
            .ok_or_else(|| invalid("A chunk's channel is not the header's"))?;
        let bpe = i32::from(ch.bytes_per_element);
        if (ch.width.max(0) * ch.height.max(0) * bpe) as usize != planes[k].len() {
            return Err(invalid(
                "A chunk's channel is not the size the header gives",
            ));
        }
        ch.user_pixel_stride = bpe;
        ch.user_line_stride = ch.width * bpe;
        ch.data = Some(BufferRef {
            buffer: k,
            offset: 0,
        });
    }
    ctx.encoding_choose_default_routines(p, &mut enc)
        .map_err(core)?;
    let ins: Vec<&[u8]> = planes.iter().map(|b| &b[..]).collect();
    let r = ctx.encoding_run(p, &mut enc, &ins).map_err(core);
    ctx.encoding_destroy(&mut enc).map_err(core)?;
    r
}

/// A tile's coordinates: `(dx, dy, lx, ly)`.
type TileCoord = (i32, i32, i32, i32);

/// A tiled part's levels and tiles, and what is left to write of them.
struct Tiles {
    mode: crate::attribute::LevelMode,
    order: LineOrder,
    num_levels: (i32, i32),
    /// Tiles in x and y of each level, by `lx` and by `ly`.
    num_x: Vec<i32>,
    num_y: Vec<i32>,
    /// The tile the file takes next (`TiledOutputFile::Data::nextTileToWrite`).
    next: TileCoord,
    written: std::collections::HashSet<TileCoord>,
    /// Tiles given ahead of their turn, their planes kept until it comes (the C++ library
    /// keeps them compressed: `bufferedTileWrite`).
    pending: std::collections::BTreeMap<TileCoord, Vec<Vec<u8>>>,
    left: u64,
}

impl Tiles {
    /// Port of `TiledOutputFile::Data::nextTileCoord`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:280-378 @ v3.5.3).
    fn next_tile_coord(&self, a: TileCoord) -> TileCoord {
        let (mut dx, mut dy, mut lx, mut ly) = a;
        let next_level = |lx: &mut i32, ly: &mut i32| match self.mode {
            crate::attribute::LevelMode::Ripmap => {
                *lx += 1;
                if *lx >= self.num_levels.0 {
                    *lx = 0;
                    *ly += 1;
                }
            }
            _ => {
                *lx += 1;
                *ly += 1;
            }
        };
        let nx = |lx: i32| self.num_x.get(lx as usize).copied().unwrap_or(0);
        let ny = |ly: i32| self.num_y.get(ly as usize).copied().unwrap_or(0);
        if self.order == LineOrder::IncreasingY {
            dx += 1;
            if dx >= nx(lx) {
                dx = 0;
                dy += 1;
                if dy >= ny(ly) {
                    dy = 0;
                    next_level(&mut lx, &mut ly);
                }
            }
        } else if self.order == LineOrder::DecreasingY {
            dx += 1;
            if dx >= nx(lx) {
                dx = 0;
                dy -= 1;
                if dy < 0 {
                    next_level(&mut lx, &mut ly);
                    if ly < self.num_levels.1 {
                        dy = ny(ly) - 1;
                    }
                }
            }
        }
        (dx, dy, lx, ly)
    }

    /// Port of `TiledOutputFile::isValidLevel`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:1664-1674 @ v3.5.3).
    fn valid_level(&self, lx: i32, ly: i32) -> bool {
        lx >= 0
            && ly >= 0
            && !(self.mode == crate::attribute::LevelMode::Mipmap && lx != ly)
            && lx < self.num_levels.0
            && ly < self.num_levels.1
    }

    fn valid_tile(&self, dx: i32, dy: i32, lx: i32, ly: i32) -> bool {
        self.valid_level(lx, ly)
            && dx >= 0
            && dy >= 0
            && dx < self.num_x[lx as usize]
            && dy < self.num_y[ly as usize]
    }
}

/// What is left to write of a part.
struct PartState {
    /// The next line to write, and the step to the one after (scanline parts).
    next_y: i32,
    step: i32,
    lines_left: i64,
    staged: Option<Staged>,
    tiles: Option<Tiles>,
}

impl PartState {
    /// Whether some of the part is not in the file yet.
    fn incomplete(&self) -> bool {
        self.lines_left > 0
            || self.staged.is_some()
            || self.tiles.as_ref().is_some_and(|t| t.left > 0)
    }
}

/// The levels and tile counts of tiled part `p`, as Core computed them writing the header.
fn part_tiles(ctx: &Context, p: i32, h: &Header) -> Result<Tiles> {
    use crate::attribute::LevelMode;
    let td = h
        .tiles()
        .ok_or_else(|| invalid("A tiled part needs tiles"))?;
    let (nlx, nly) = ctx.get_tile_levels(p).map_err(core)?;
    let rip = td.level_mode == LevelMode::Ripmap;
    let mut num_x = Vec::new();
    for lx in 0..nlx {
        let ly = if rip { 0 } else { lx.min(nly - 1) };
        num_x.push(ctx.get_tile_counts(p, lx, ly).map_err(core)?.0);
    }
    let mut num_y = Vec::new();
    for ly in 0..nly {
        let lx = if rip { 0 } else { ly.min(nlx - 1) };
        num_y.push(ctx.get_tile_counts(p, lx, ly).map_err(core)?.1);
    }
    let order = h.line_order();
    // TiledOutputFile::initialize (src/lib/OpenEXR/ImfTiledOutputFile.cpp:995-997 @ v3.5.3)
    let next = if order == LineOrder::IncreasingY {
        (0, 0, 0, 0)
    } else {
        (0, num_y[0] - 1, 0, 0)
    };
    Ok(Tiles {
        mode: td.level_mode,
        order,
        num_levels: (nlx, nly),
        num_x,
        num_y,
        next,
        written: Default::default(),
        pending: Default::default(),
        left: ctx.get_chunk_count(p).map_err(core)? as u64,
    })
}

/// A file being written, every part of it: the C++ library's `MultiPartOutputFile`.
///
/// Parts are written one after another, each in its line order, as Core writes them (the
/// C++ library also takes parts in any order); [`finish`](Self::finish) ends the file and
/// reports what went wrong, which dropping cannot.
pub struct MultiPartOutputFile<W: Write + Seek + Send + 'static = std::fs::File> {
    ctx: Option<Context>,
    sink: Arc<Mutex<Sink<W>>>,
    headers: Vec<Header>,
    parts: Vec<PartState>,
}

impl<W: Write + Seek + Send + 'static> std::fmt::Debug for MultiPartOutputFile<W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiPartOutputFile")
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

impl MultiPartOutputFile<std::fs::File> {
    /// Creates the file at `path` for `headers`, with the default [`WriteOptions`].
    pub fn create(path: impl AsRef<Path>, headers: &[Header]) -> Result<Self> {
        WriteOptions::default().create(path, headers)
    }
}

impl<W: Write + Seek + Send + 'static> MultiPartOutputFile<W> {
    fn start(name: &str, writer: W, headers: &[Header], opts: &WriteOptions) -> Result<Self> {
        if headers.is_empty() {
            return Err(invalid("A file needs at least one part"));
        }
        let sink = Arc::new(Mutex::new(Sink {
            w: writer,
            state: State::Header(Vec::new()),
        }));
        let into = Arc::clone(&sink);
        let mut init = Initializer {
            error_handler: Some(Arc::new(|_| {})),
            write_fn: Some(Box::new(move |errs, buf, off| {
                into.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .write(errs, buf, off)
            })),
            flags: EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER,
            ..Initializer::default()
        };
        if let Some(l) = opts.zip_level {
            init.zip_level = l;
        }
        if let Some(l) = opts.zstd_level {
            init.zstd_level = l;
        }
        if let Some(q) = opts.dwa_quality {
            init.dwa_quality = q;
        }
        let mut ctx =
            Context::start_write(Some(name), DefaultWriteMode::WriteFileDirectly, Some(init))
                .map_err(core)?;
        for h in headers {
            let storage = core_storage(h.storage())?;
            if let Some(t) = h.part_type()
                && t != type_string(h.storage())
            {
                return Err(invalid(format!(
                    "The header's type \"{}\" is not its storage's",
                    String::from_utf8_lossy(t)
                )));
            }
            let pname = h.name().map(|n| utf8(n, "The part's name")).transpose()?;
            let p = ctx.add_part(pname, storage).map_err(core)?;
            for (name, a) in h.iter() {
                match name {
                    b"type" | b"name" | b"chunkCount" => continue,
                    b"version" if h.storage().is_deep() => {
                        if let Attribute::Int(v) = a {
                            ctx.set_version(p, *v).map_err(core)?;
                        }
                        continue;
                    }
                    _ => {}
                }
                set_attribute(&mut ctx, p, utf8(name, "An attribute name")?, a)?;
            }
        }
        ctx.write_header().map_err(core)?;

        // the header as the C++ library writes it, then everything after it moved
        {
            let mut s = sink.lock().unwrap_or_else(|p| p.into_inner());
            let State::Header(core_header) = std::mem::replace(
                &mut s.state,
                State::Body {
                    delta: 0,
                    table: (0, 0),
                },
            ) else {
                unreachable!("written once")
            };
            let h = &headers[0];
            let mut inserts: Vec<(&[u8], &[u8])> = Vec::new();
            if headers.len() == 1 && !h.storage().is_deep() {
                if let Some(t) = h.part_type() {
                    inserts.push((b"type", t));
                }
                if let Some(n) = h.name() {
                    inserts.push((b"name", n));
                }
            }
            let bytes = if inserts.is_empty() {
                core_header.clone()
            } else {
                cpp_header(&core_header, &inserts)
            };
            let delta = (bytes.len() - core_header.len()) as u64;
            let table_at = ctx.get_chunk_table_offset(0).map_err(core)?;
            let table_len = ctx.get_chunk_count(0).map_err(core)? as usize * 8;
            s.w.seek(SeekFrom::Start(0))
                .and_then(|_| s.w.write_all(&bytes))
                .map_err(|e| {
                    Error::new(
                        ErrorCode::WriteIo,
                        format!("Unable to write the header: {e}"),
                    )
                })?;
            s.state = State::Body {
                delta,
                table: (table_at, table_len),
            };
        }

        let parts = headers
            .iter()
            .enumerate()
            .map(|(p, h)| {
                let dw = h.data_window();
                let increasing = h.line_order() == LineOrder::IncreasingY;
                let tiles = if h.storage().is_tiled() {
                    Some(part_tiles(&ctx, p as i32, h)?)
                } else {
                    None
                };
                Ok(PartState {
                    // OutputFile::initialize (src/lib/OpenEXR/ImfOutputFile.cpp:779-781
                    // @ v3.5.3): any order but increasing writes from the bottom
                    next_y: if increasing { dw.min.y } else { dw.max.y },
                    step: if increasing { 1 } else { -1 },
                    lines_left: if tiles.is_some() {
                        0
                    } else {
                        i64::from(dw.max.y) - i64::from(dw.min.y) + 1
                    },
                    staged: None,
                    tiles,
                })
            })
            .collect::<Result<_>>()?;
        Ok(MultiPartOutputFile {
            ctx: Some(ctx),
            sink,
            headers: headers.to_vec(),
            parts,
        })
    }

    /// The parts' headers.
    pub fn headers(&self) -> &[Header] {
        &self.headers
    }

    /// Part `index`, to write.
    pub fn part(&mut self, index: usize) -> Result<OutputPart<'_, W>> {
        if index >= self.headers.len() {
            return Err(Error::new(
                ErrorCode::ArgumentOutOfRange,
                format!("Part index ({index}) out of range"),
            ));
        }
        Ok(OutputPart { file: self, index })
    }

    /// Ends the file and gives the writer back. Fails if a part is not whole, or a write
    /// failed.
    pub fn finish(mut self) -> Result<W> {
        let ctx = self.ctx.take().expect("not finished");
        let unwritten = self.parts.iter().position(PartState::incomplete);
        let rv = ctx.finish().map_err(core);
        if let Some(p) = unwritten {
            return Err(Error::new(
                ErrorCode::IncompleteChunkTable,
                format!("Part {p} was not written whole"),
            ));
        }
        rv?;
        let sink = Arc::try_unwrap(self.sink)
            .map_err(|_| invalid("The writer is still in use"))?
            .into_inner()
            .unwrap_or_else(|p| p.into_inner());
        Ok(sink.w)
    }
}

impl<W: Write + Seek + Send + 'static> std::fmt::Debug for OutputPart<'_, W> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPart")
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

/// One part of a file being written: the C++ library's `OutputPart`.
pub struct OutputPart<'f, W: Write + Seek + Send + 'static> {
    file: &'f mut MultiPartOutputFile<W>,
    index: usize,
}

impl<W: Write + Seek + Send + 'static> OutputPart<'_, W> {
    /// The part's header.
    pub fn header(&self) -> &Header {
        &self.file.headers[self.index]
    }

    /// The number of tiles of level `level` in x and y (a tiled part).
    pub fn num_tiles(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        let tiles = self.file.parts[self.index]
            .tiles
            .as_ref()
            .ok_or_else(|| invalid("The part is not tiled"))?;
        if !tiles.valid_level(level.0, level.1) {
            return Err(invalid(format!(
                "Level coordinate ({}, {}) is invalid.",
                level.0, level.1
            )));
        }
        Ok((tiles.num_x[level.0 as usize], tiles.num_y[level.1 as usize]))
    }

    /// The width and height of level `level` (a tiled part).
    pub fn level_size(&self, level: (i32, i32)) -> Result<(i32, i32)> {
        self.num_tiles(level)?;
        let ctx = self.file.ctx.as_ref().expect("not finished");
        ctx.get_level_sizes(self.index as i32, level.0, level.1)
            .map_err(core)
    }

    /// The next line [`write_pixels`](Self::write_pixels) writes.
    pub fn current_scan_line(&self) -> i32 {
        self.file.parts[self.index].next_y
    }

    /// Writes the next `lines` lines of a scanline part from `fb`, in the part's line order
    /// (from the top for increasing Y, from the bottom otherwise): port of
    /// `OutputFile::writePixels` (src/lib/OpenEXR/ImfOutputFile.cpp:987-1160 @ v3.5.3). A
    /// channel without a slice in `fb` is written as zeros; a slice must have its channel's
    /// type and sampling (`setFrameBuffer`, lines 876-985). A chunk goes to the file once
    /// all its lines are given.
    pub fn write_pixels(&mut self, fb: &FrameBuffer<'_>, lines: i32) -> Result<()> {
        let h = &self.file.headers[self.index];
        if h.storage() != Storage::Scanline {
            return Err(invalid("write_pixels writes scanline parts"));
        }
        // Core takes the parts one after another (chunk.c, `cur_output_part`): refused here
        // before anything is staged
        if self.file.parts[..self.index]
            .iter()
            .any(PartState::incomplete)
        {
            return Err(Error::new(
                ErrorCode::IncorrectPart,
                openexr_core::base::get_default_error_message(ErrorCode::IncorrectPart.code()),
            ));
        }
        let state = &self.file.parts[self.index];
        if i64::from(lines) > state.lines_left {
            return Err(invalid(
                "Tried to write more scan lines than specified by the data window.",
            ));
        }
        if lines <= 0 {
            return Ok(());
        }
        let dw = h.data_window();
        for c in h.channels() {
            let Some(s) = fb.slice(&c.name) else { continue };
            let cname = String::from_utf8_lossy(&c.name);
            if s.pixel_type != c.pixel_type {
                return Err(invalid(format!(
                    "Pixel type of \"{cname}\" channel is not compatible with the frame buffer's pixel type."
                )));
            }
            if s.x_sampling != c.x_sampling || s.y_sampling != c.y_sampling {
                return Err(invalid(format!(
                    "X and/or y subsampling factors of \"{cname}\" channel are not compatible with the frame buffer's subsampling factors."
                )));
            }
            let a = state.next_y;
            let b = a + state.step * (lines - 1);
            check_slice(fb, &c.name, s, dw.min.x, dw.max.x, a.min(b), a.max(b))?;
        }
        for _ in 0..lines {
            self.stage_line(fb)?;
        }
        Ok(())
    }

    /// Writes tile `(dx, dy)` of level `level` of a tiled part from `fb`
    /// ([`write_tiles`](Self::write_tiles)).
    pub fn write_tile(
        &mut self,
        fb: &FrameBuffer<'_>,
        dx: i32,
        dy: i32,
        level: (i32, i32),
    ) -> Result<()> {
        self.write_tiles(fb, (dx, dx), (dy, dy), level)
    }

    /// Writes tiles `dx` (first, last) by `dy` of `level` of a tiled part from `fb`, whose
    /// slices address the level's pixels: port of `TiledOutputFile::writeTiles`
    /// (src/lib/OpenEXR/ImfTiledOutputFile.cpp:1191-1395 @ v3.5.3), a row at a time in the
    /// part's line order. A tile given before its turn (in increasing or decreasing Y) is
    /// kept until the tiles before it are written, as `bufferedTileWrite` keeps it (lines
    /// 442-546); a tile given twice is refused.
    pub fn write_tiles(
        &mut self,
        fb: &FrameBuffer<'_>,
        dx: (i32, i32),
        dy: (i32, i32),
        level: (i32, i32),
    ) -> Result<()> {
        if self.file.parts[..self.index]
            .iter()
            .any(PartState::incomplete)
        {
            return Err(Error::new(
                ErrorCode::IncorrectPart,
                openexr_core::base::get_default_error_message(ErrorCode::IncorrectPart.code()),
            ));
        }
        let h = &self.file.headers[self.index];
        let Some(tiles) = self.file.parts[self.index].tiles.as_ref() else {
            return Err(invalid("write_tiles writes tiled parts"));
        };
        let (lx, ly) = level;
        if !tiles.valid_tile(dx.0, dy.0, lx, ly) || !tiles.valid_tile(dx.1, dy.1, lx, ly) {
            return Err(invalid("Tile coordinates are invalid."));
        }
        let dx = if dx.1 < dx.0 { (dx.1, dx.0) } else { dx };
        let dy = if dy.1 < dy.0 { (dy.1, dy.0) } else { dy };
        let decreasing = tiles.order == LineOrder::DecreasingY;
        let p = self.index as i32;
        let ctx = self.file.ctx.as_ref().expect("not finished");
        let (tw, th) = ctx.get_tile_sizes(p, lx, ly).map_err(core)?;
        let (lw, lh) = ctx.get_level_sizes(p, lx, ly).map_err(core)?;
        let dw = h.data_window();
        let tile_window = |tx: i32, ty: i32| {
            let x0 = dw.min.x + tx * tw;
            let y0 = dw.min.y + ty * th;
            (
                x0,
                (x0 + tw - 1).min(dw.min.x + lw - 1),
                y0,
                (y0 + th - 1).min(dw.min.y + lh - 1),
            )
        };
        for c in h.channels() {
            let Some(s) = fb.slice(&c.name) else {
                continue;
            };
            let cname = String::from_utf8_lossy(&c.name);
            if s.pixel_type != c.pixel_type {
                return Err(invalid(format!(
                    "Pixel type of \"{cname}\" channel is not compatible with the frame buffer's pixel type."
                )));
            }
            if s.x_sampling != 1 || s.y_sampling != 1 {
                return Err(invalid(format!(
                    "X and/or y subsampling factors of \"{cname}\" channel are not compatible with the frame buffer's subsampling factors."
                )));
            }
            let (x1, _, y1, _) = tile_window(dx.0, dy.0);
            let (_, x2, _, y2) = tile_window(dx.1, dy.1);
            check_slice(fb, &c.name, s, x1, x2, y1, y2)?;
        }
        let rows: Vec<i32> = if decreasing {
            (dy.0..=dy.1).rev().collect()
        } else {
            (dy.0..=dy.1).collect()
        };
        for ty in rows {
            for tx in dx.0..=dx.1 {
                let (x0, x1, y0, y1) = tile_window(tx, ty);
                let h = &self.file.headers[self.index];
                let planes: Vec<Vec<u8>> = h
                    .channels()
                    .iter()
                    .map(|c| {
                        let size = c.pixel_type.size();
                        let w = (x1 - x0 + 1) as usize;
                        let mut plane = vec![0u8; w * (y1 - y0 + 1) as usize * size];
                        if let Some(s) = fb.slice(&c.name) {
                            let src = fb.buffer(s.buffer);
                            for y in y0..=y1 {
                                for x in x0..=x1 {
                                    // checked by check_slice
                                    let o = s.offset_of(x, y).expect("checked") as usize;
                                    let d = ((y - y0) as usize * w + (x - x0) as usize) * size;
                                    plane[d..d + size].copy_from_slice(&src[o..o + size]);
                                }
                            }
                        }
                        plane
                    })
                    .collect();
                self.put_tile((tx, ty, lx, ly), planes)?;
            }
        }
        Ok(())
    }

    /// Port of `bufferedTileWrite` (src/lib/OpenEXR/ImfTiledOutputFile.cpp:442-546
    /// @ v3.5.3): the tile goes to the file if its turn has come, then the kept tiles whose
    /// turn follows; else it is kept.
    fn put_tile(&mut self, at: TileCoord, planes: Vec<Vec<u8>>) -> Result<()> {
        let p = self.index as i32;
        let file = &mut *self.file;
        let h = &file.headers[self.index];
        let ctx = file.ctx.as_mut().expect("not finished");
        let tiles = file.parts[self.index].tiles.as_mut().expect("tiled");
        if tiles.written.contains(&at) || tiles.pending.contains_key(&at) {
            return Err(invalid(format!(
                "Attempt to write tile ({}, {}, {}, {}) more than once.",
                at.0, at.1, at.2, at.3
            )));
        }
        let write = |ctx: &mut Context, at: TileCoord, planes: &[Vec<u8>]| -> Result<()> {
            let winfo = ctx
                .write_tile_chunk_info(p, at.0, at.1, at.2, at.3)
                .map_err(core)?;
            encode_chunk(ctx, p, &winfo, h, planes)
        };
        if tiles.order == LineOrder::RandomY {
            write(ctx, at, &planes)?;
            tiles.written.insert(at);
            tiles.left -= 1;
            return Ok(());
        }
        if tiles.next != at {
            tiles.pending.insert(at, planes);
            return Ok(());
        }
        write(ctx, at, &planes)?;
        tiles.written.insert(at);
        tiles.left -= 1;
        tiles.next = tiles.next_tile_coord(tiles.next);
        while let Some(planes) = tiles.pending.remove(&tiles.next) {
            let at = tiles.next;
            write(ctx, at, &planes)?;
            tiles.written.insert(at);
            tiles.left -= 1;
            tiles.next = tiles.next_tile_coord(tiles.next);
        }
        Ok(())
    }

    /// Copies the next line into its chunk's staging, and writes the chunk once whole.
    fn stage_line(&mut self, fb: &FrameBuffer<'_>) -> Result<()> {
        let p = self.index as i32;
        let file = &mut *self.file;
        let ctx = file.ctx.as_mut().expect("not finished");
        let h = &file.headers[self.index];
        let dw = h.data_window();
        let state = &mut file.parts[self.index];
        let y = state.next_y;
        if state.staged.is_none() {
            let lpc = ctx.get_scanlines_per_chunk(p).map_err(core)?;
            let first = dw.min.y + (y - dw.min.y) / lpc * lpc;
            let last = (first + lpc - 1).min(dw.max.y);
            let planes = h
                .channels()
                .iter()
                .map(|c| {
                    vec![
                        0u8;
                        sample_count(dw.min.x, dw.max.x, c.x_sampling)
                            * sample_count(first, last, c.y_sampling)
                            * c.pixel_type.size()
                    ]
                })
                .collect();
            state.staged = Some(Staged {
                first,
                last,
                lines: 0,
                planes,
            });
        }
        let staged = state.staged.as_mut().expect("staged");
        for (c, plane) in h.channels().iter().zip(staged.planes.iter_mut()) {
            let Some(s) = fb.slice(&c.name) else { continue };
            if y % c.y_sampling != 0 {
                continue;
            }
            let size = c.pixel_type.size();
            let w = sample_count(dw.min.x, dw.max.x, c.x_sampling);
            let row = sample_count(staged.first, y - 1, c.y_sampling);
            let src = fb.buffer(s.buffer);
            let x0 = first_multiple(dw.min.x, c.x_sampling) as i32;
            for i in 0..w {
                // checked by check_slice
                let o = s
                    .offset_of(x0 + i as i32 * c.x_sampling, y)
                    .expect("checked") as usize;
                let d = (row * w + i) * size;
                plane[d..d + size].copy_from_slice(&src[o..o + size]);
            }
        }
        staged.lines += 1;
        state.next_y += state.step;
        state.lines_left -= 1;
        if staged.lines == staged.last - staged.first + 1 {
            let staged = state.staged.take().expect("staged");
            let winfo = ctx
                .write_scanline_chunk_info(p, staged.first)
                .map_err(core)?;
            encode_chunk(ctx, p, &winfo, h, &staged.planes)?;
        }
        Ok(())
    }
}

/// A file's single part, being written: the C++ library's `OutputFile`.
#[derive(Debug)]
pub struct OutputFile<W: Write + Seek + Send + 'static = std::fs::File> {
    file: MultiPartOutputFile<W>,
}

impl OutputFile<std::fs::File> {
    /// Creates the file at `path` with one part, `header`.
    pub fn create(path: impl AsRef<Path>, header: &Header) -> Result<Self> {
        Ok(OutputFile {
            file: MultiPartOutputFile::create(path, std::slice::from_ref(header))?,
        })
    }
}

impl<W: Write + Seek + Send + 'static> OutputFile<W> {
    /// The part's header.
    pub fn header(&self) -> &Header {
        &self.file.headers[0]
    }

    /// Writes the next `lines` lines from `fb` ([`OutputPart::write_pixels`]).
    pub fn write_pixels(&mut self, fb: &FrameBuffer<'_>, lines: i32) -> Result<()> {
        self.file.part(0)?.write_pixels(fb, lines)
    }

    /// Ends the file ([`MultiPartOutputFile::finish`]).
    pub fn finish(self) -> Result<W> {
        self.file.finish()
    }
}

impl<W: Write + Seek + Send + 'static> From<MultiPartOutputFile<W>> for OutputFile<W> {
    fn from(file: MultiPartOutputFile<W>) -> Self {
        OutputFile { file }
    }
}
