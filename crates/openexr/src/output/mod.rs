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
use openexr_core::cfmt::Arg;
use openexr_core::context::{
    DefaultWriteMode, EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER, Initializer, StreamErrors,
};

use crate::attribute::{Attribute, LineOrder, Storage};
use crate::error::{Error, ErrorCode, Result};
use crate::frame_buffer::{FrameBuffer, first_multiple, sample_count};
use crate::header::Header;
use crate::parallel::{Pool, Threads};
use crate::read::check_slice;

mod attributes;
mod deep;
mod tiles;

use attributes::{core_storage, set_attribute, type_string, utf8};
use tiles::{TileCoord, Tiles, part_tiles};

fn core(e: openexr_core::Error) -> Error {
    Error::from_core(&e)
}

fn invalid(msg: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, msg)
}

/// How a file is written: Core's compression settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteOptions {
    zip_level: Option<i32>,
    zstd_level: Option<i32>,
    /// The DWA quality's bits (an `f32`), so that the options compare and hash.
    dwa_quality: Option<u32>,
    threads: Threads,
}

impl WriteOptions {
    /// The defaults: Core's levels, which the C++ library's are.
    pub fn new() -> WriteOptions {
        WriteOptions::default()
    }

    /// The threads a write compresses its chunks on ([`Threads`]; single-threaded by
    /// default). Deep parts compress one chunk after another.
    pub fn threads(mut self, threads: Threads) -> WriteOptions {
        self.threads = threads;
        self
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
        self.dwa_quality = Some(quality.to_bits());
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

/// A chunk of lines on its way to the file: each channel's samples, in the file's type, as
/// the C++ library's line buffer holds them until the chunk is whole.
struct Staged {
    first: i32,
    last: i32,
    lines: i32,
    planes: Vec<Vec<u8>>,
}

/// The bytes Core's encode pipeline writes for chunk `winfo` of part `p`, whose channels'
/// samples `planes` holds (one per channel of `h`, in the file's type, row by row): the
/// chunk packed as Core's pack stage packs planar samples of the file's types (each line,
/// each channel's samples of it, little-endian: `internal_exr_match_encode`,
/// src/lib/OpenEXRCore/pack.c @ v3.5.3, checked against the wheel's files by
/// tests/heavy_write.rs), then compressed by `exr_compress_chunk` (the compress stage,
/// src/lib/OpenEXRCore/encoding.c:291-297 @ v3.5.3), or left packed without a compression.
/// It takes Core's context shared, so chunks compress on several threads at once; the
/// write stage, which takes it alone, comes after ([`write_flat`]).
fn compress_flat(
    ctx: &Context,
    p: i32,
    winfo: &openexr_core::chunk_info::ChunkInfo,
    h: &Header,
    planes: &[Vec<u8>],
    state: &mut EncodeState,
) -> Result<Vec<u8>> {
    // one pipeline from chunk to chunk (`exr_encoding_update`), as Core's callers keep one,
    // so that its buffers, the compressed one sized for the worst case, are made once
    let enc = match &mut state.enc {
        Some(enc) => {
            ctx.encoding_update(p, winfo, enc).map_err(core)?;
            enc
        }
        slot @ None => slot.insert(ctx.encoding_initialize(p, winfo).map_err(core)?),
    };
    let mut packed = std::mem::take(&mut state.packed);
    packed.clear();
    packed.reserve(winfo.unpacked_size as usize);
    let mut rows_done = vec![0usize; planes.len()];
    let scanline = h.storage() == Storage::Scanline;
    let mut keys = Vec::with_capacity(enc.channels.len());
    for ch in &enc.channels {
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
        keys.push(k);
    }
    for r in 0..winfo.height {
        for (ch, &k) in enc.channels.iter().zip(&keys) {
            if ch.height <= 0 {
                continue;
            }
            if scanline && ch.y_samples > 1 && (winfo.start_y + r) % ch.y_samples != 0 {
                continue;
            }
            let bpe = ch.bytes_per_element as usize;
            let line = ch.width.max(0) as usize * bpe;
            let row = &planes[k][rows_done[k] * line..(rows_done[k] + 1) * line];
            rows_done[k] += 1;
            if cfg!(target_endian = "little") {
                packed.extend_from_slice(row);
            } else {
                for v in row.chunks_exact(bpe) {
                    packed.extend(v.iter().rev());
                }
            }
        }
    }
    if packed.len() as u64 != winfo.unpacked_size {
        return Err(invalid("A chunk packs to another size than Core gives it"));
    }
    if h.compression() == crate::attribute::Compression::None {
        return Ok(packed);
    }
    enc.packed_bytes = packed.len() as u64;
    enc.packed_buffer = Some(packed);
    let r = openexr_core::compression::compress_chunk(enc, ctx).map_err(core);
    let out = enc
        .compressed()
        .get(..enc.compressed_bytes)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| invalid("A compressed chunk is shorter than its size"));
    // the packed bytes come back for the next chunk, not into Core's buffer pool
    state.packed = enc.packed_buffer.take().unwrap_or_default();
    r?;
    out
}

/// Copies `dst.len() / size` samples of `size` bytes from `src`, the first at byte `from`
/// and each `step` bytes after the one before, to `dst` one after another: one copy where
/// they are side by side. The samples were checked to lie in `src`.
pub(super) fn gather(dst: &mut [u8], src: &[u8], from: usize, step: i64, size: usize) {
    if step == size as i64 {
        dst.copy_from_slice(&src[from..from + dst.len()]);
        return;
    }
    let mut o = from as i64;
    for d in dst.chunks_exact_mut(size) {
        let u = o as usize;
        d.copy_from_slice(&src[u..u + size]);
        o += step;
    }
}

/// What a thread keeps from chunk to chunk it compresses: Core's encode pipeline and the
/// buffer chunks are packed into.
#[derive(Default)]
struct EncodeState {
    enc: Option<openexr_core::encoding::EncodePipeline>,
    packed: Vec<u8>,
}

/// Where a flat chunk goes: its first line, or its tile.
#[derive(Debug, Clone, Copy)]
enum ChunkAt {
    Scan(i32),
    Tile(TileCoord),
}

/// The chunk info of chunk `at` of part `p`, as the write side gives it.
fn write_info(ctx: &Context, p: i32, at: ChunkAt) -> Result<openexr_core::chunk_info::ChunkInfo> {
    match at {
        ChunkAt::Scan(y) => ctx.write_scanline_chunk_info(p, y),
        ChunkAt::Tile((tx, ty, lx, ly)) => ctx.write_tile_chunk_info(p, tx, ty, lx, ly),
    }
    .map_err(core)
}

/// Compresses `chunks` of part `p` on `pool`'s threads ([`compress_flat`]): each chunk's
/// bytes, in order.
fn compress_all(
    pool: &Pool,
    ctx: &Context,
    p: i32,
    h: &Header,
    chunks: Vec<(ChunkAt, Vec<Vec<u8>>)>,
) -> Vec<(ChunkAt, Result<Vec<u8>>)> {
    pool.map_init(chunks, EncodeState::default, |state, (at, planes)| {
        let r = write_info(ctx, p, at)
            .and_then(|winfo| compress_flat(ctx, p, &winfo, h, &planes, state));
        (at, r)
    })
}

/// Writes a compressed flat chunk: Core's write stage (`default_write_chunk`,
/// src/lib/OpenEXRCore/encoding.c:31-94 @ v3.5.3).
fn write_flat(ctx: &mut Context, p: i32, at: ChunkAt, bytes: &[u8]) -> Result<()> {
    match at {
        ChunkAt::Scan(y) => ctx.write_scanline_chunk(p, y, Some(bytes)),
        ChunkAt::Tile((tx, ty, lx, ly)) => ctx.write_tile_chunk(p, tx, ty, lx, ly, Some(bytes)),
    }
    .map_err(core)
}

/// What is left to write of a part.
struct PartState {
    /// The next line to write, and the step to the one after (scanline parts).
    next_y: i32,
    step: i32,
    lines_left: i64,
    staged: Option<Staged>,
    tiles: Option<Tiles>,
    /// A deep scanline part's lines given toward the next chunk.
    deep_lines: Option<deep::DeepLines>,
    /// A deep tiled part's sample count table buffer, reused from tile to tile.
    deep_table: Vec<u8>,
}

impl PartState {
    /// Whether some of the part is not in the file yet.
    fn incomplete(&self) -> bool {
        self.lines_left > 0
            || self.staged.is_some()
            || self.deep_lines.is_some()
            || self.tiles.as_ref().is_some_and(|t| t.left > 0)
    }
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
    pool: Pool,
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
            init.dwa_quality = f32::from_bits(q);
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
                    deep_lines: None,
                    deep_table: h.tiles().map_or(Vec::new(), |t| {
                        vec![0u8; t.x_size as usize * t.y_size as usize * 4]
                    }),
                })
            })
            .collect::<Result<_>>()?;
        Ok(MultiPartOutputFile {
            ctx: Some(ctx),
            sink,
            headers: headers.to_vec(),
            parts,
            pool: Pool::new(opts.threads)?,
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
        let mut whole = Vec::new();
        for _ in 0..lines {
            if let Some(chunk) = self.stage_line(fb)? {
                whole.push((ChunkAt::Scan(chunk.first), chunk.planes));
            }
        }
        // the chunks completed compress on the file's threads, then go to the file in order
        let p = self.index as i32;
        let file = &mut *self.file;
        let h = &file.headers[self.index];
        let ctx = file.ctx.as_ref().expect("not finished");
        let done = compress_all(&file.pool, ctx, p, h, whole);
        let ctx = file.ctx.as_mut().expect("not finished");
        for (at, bytes) in done {
            write_flat(ctx, p, at, &bytes?)?;
        }
        Ok(())
    }

    /// Copies the next line into its chunk's staging; gives the chunk once whole.
    fn stage_line(&mut self, fb: &FrameBuffer<'_>) -> Result<Option<Staged>> {
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
            let x0 = first_multiple(dw.min.x, c.x_sampling) as i32;
            // checked by check_slice; the next sample is `x_stride` on
            let from = s.offset_of(x0, y).expect("checked") as usize;
            gather(
                &mut plane[row * w * size..(row + 1) * w * size],
                fb.buffer(s.buffer),
                from,
                s.x_stride,
                size,
            );
        }
        staged.lines += 1;
        state.next_y += state.step;
        state.lines_left -= 1;
        if staged.lines == staged.last - staged.first + 1 {
            return Ok(state.staged.take());
        }
        Ok(None)
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
