// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, O1): the port against the source-built Core harness
//! (`oracle/native/core_harness.c`), OpenEXRCore 3.5.3 itself, where the wheel can't reach it and
//! the harness reproduces the wheel (`docs/baseline.md`).
//!
//! - Reading: every image of upstream's test tree and of `oracle/write_images.py`, whole and
//!   damaged (`oracle/native_accept_messages.py`'s copies), opened and read chunk by chunk as
//!   `core_harness info --read` does: the per-chunk results and Core's messages, line for
//!   line; and `exr_print_context_info`'s verbose text, byte for byte.
//! - Writing: every layout of `oracle/write_images.py`, rewritten by Core alone from its
//!   uncompressed copy (`core_harness recompress`) into each compression, ZIP and ZIPS at
//!   every level, ZSTD at every level the port compresses, DWA at a few: the header byte for
//!   byte, and every chunk, by index.
//!
//! The port's deviations from upstream (`docs/deviations.md`) are left out of the comparison
//! one by one, each where it applies, with its number.
//!
//! The harness is built by `scripts/oracle-native-build.cmd` (Windows) or
//! `scripts/oracle-native-build.sh` (Linux).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use openexr_core::base::get_error_code_as_string;
use openexr_core::coding::BufferRef;
use openexr_core::context::{
    Context, DefaultWriteMode, EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE, Error, ErrorReport,
    Initializer, default_error_line,
};
use openexr_core::openexr_attr::{Compression, LineOrder, PixelType, Storage, TileLevelMode};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The harness, which the heavy tier needs built.
fn harness() -> PathBuf {
    let p = if cfg!(windows) {
        root().join("target/oracle-native/msvc-Release/core_harness.exe")
    } else {
        root().join("target/rocky9/oracle-native/gcc-Release/core_harness")
    };
    assert!(
        p.exists(),
        "{} is missing: build it with scripts/oracle-native-build",
        p.display()
    );
    p
}

fn oracle_python() -> Command {
    let mut c = Command::new("uv");
    c.args(["run", "--project"])
        .arg(root().join("oracle"))
        .args(["python", "-I"]);
    c
}

/// The lines of `bytes`, each without its newline.
fn lines(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = bytes
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l).to_vec())
        .collect();
    if v.last().is_some_and(|l| l.is_empty()) {
        v.pop();
    }
    v
}

/// `oracle/write_images.py`'s images, every compression, in `dir`.
fn write_images(dir: &Path) {
    let status = oracle_python()
        .arg(root().join("oracle/write_images.py"))
        .arg(dir)
        .args(COMPRESSIONS.iter().map(|c| c.1))
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "write_images.py failed");
}

const COMPRESSIONS: [(Compression, &str); 14] = [
    (Compression::NONE, "NO_COMPRESSION"),
    (Compression::RLE, "RLE_COMPRESSION"),
    (Compression::ZIPS, "ZIPS_COMPRESSION"),
    (Compression::ZIP, "ZIP_COMPRESSION"),
    (Compression::PIZ, "PIZ_COMPRESSION"),
    (Compression::PXR24, "PXR24_COMPRESSION"),
    (Compression::B44, "B44_COMPRESSION"),
    (Compression::B44A, "B44A_COMPRESSION"),
    (Compression::DWAA, "DWAA_COMPRESSION"),
    (Compression::DWAB, "DWAB_COMPRESSION"),
    (Compression::HTJ2K256, "HTJ2K256_COMPRESSION"),
    (Compression::HTJ2K32, "HTJ2K32_COMPRESSION"),
    (Compression::LJ2K, "LJ2K_COMPRESSION"),
    (Compression::ZSTD, "ZSTD_COMPRESSION"),
];

fn exr_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("dir")
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            exr_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "exr") {
            out.push(p);
        }
    }
}

/// The images to read: upstream's test tree and `write_images.py`'s, and their damaged copies.
fn read_set(scratch: &Path) -> Vec<PathBuf> {
    let written = scratch.join("written");
    let damage = scratch.join("damaged");
    std::fs::create_dir_all(&written).unwrap();
    std::fs::create_dir_all(&damage).unwrap();
    write_images(&written);
    let mut files = Vec::new();
    exr_files(&root().join("upstream/openexr/src/test"), &mut files);
    exr_files(&written, &mut files);
    // the damaged copies native_accept_messages.py checks the harness on
    // (the list in a file: a command line of every path is too long for Windows)
    let list = scratch.join("files.txt");
    let names: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    std::fs::write(&list, names.join("\n")).unwrap();
    let script = "import sys\nsys.path.insert(0, sys.argv[1])\nimport native_accept_messages as m\nfor p in open(sys.argv[3], encoding='utf-8').read().split(chr(10)):\n    m.damaged(p, sys.argv[2])\n";
    let status = oracle_python()
        .args(["-c", script])
        .arg(root().join("oracle"))
        .arg(&damage)
        .arg(&list)
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "the damaged copies");
    exr_files(&damage, &mut files);
    files
}

/// What `core_harness info` gives: its stdout and stderr lines.
struct Info {
    out: Vec<Vec<u8>>,
    err: Vec<Vec<u8>>,
}

fn harness_info(path: &str, args: &[&str]) -> Info {
    let o = Command::new(harness())
        .arg("info")
        .arg(path)
        .args(args)
        .output()
        .expect("the harness runs");
    assert!(o.status.success(), "core_harness info {path} failed");
    Info {
        out: lines(&o.stdout),
        err: lines(&o.stderr),
    }
}

/// A context's messages, as Core's default error handler prints them.
type Messages = Arc<Mutex<Vec<u8>>>;

fn capture(flags: i32) -> (Initializer, Messages) {
    let msgs: Messages = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&msgs);
    let init = Initializer {
        error_handler: Some(Arc::new(move |r: &ErrorReport<'_>| {
            sink.lock().unwrap().extend(default_error_line(r))
        })),
        flags,
        ..Initializer::default()
    };
    (init, msgs)
}

fn code(r: &Result<(), Error>) -> &'static str {
    match r {
        Ok(()) => "EXR_ERR_SUCCESS",
        Err(e) => get_error_code_as_string(e.code),
    }
}

/// `read_chunk` of the harness: one chunk decoded without outputs.
fn read_chunk(
    f: &Context,
    p: i32,
    ci: &openexr_core::chunk_info::ChunkInfo,
    out: &mut Vec<Vec<u8>>,
) {
    let mut digest = None;
    let rv = (|| {
        let mut dec = f.decoding_initialize(p, ci)?;
        let r = f
            .decoding_choose_default_routines(p, &mut dec)
            .and_then(|()| f.decoding_run(p, &mut dec, &mut []));
        if r.is_ok() && !dec.unpacked_is_null() {
            // the harness's fingerprint of the unpacked bytes: FNV-1a 64
            let u = dec.unpacked();
            let mut h: u64 = 14695981039346656037;
            for &b in &u[..(ci.unpacked_size as usize).min(u.len())] {
                h ^= u64::from(b);
                h = h.wrapping_mul(1099511628211);
            }
            digest = Some(h);
        }
        let _ = f.decoding_destroy(&mut dec);
        r
    })();
    out.push(match digest {
        Some(h) => format!("chunk {p} {} {} {h:016x}", ci.idx, code(&rv)).into_bytes(),
        None => format!("chunk {p} {} {}", ci.idx, code(&rv)).into_bytes(),
    });
}

/// `read_all` of the harness: every chunk of every part, in chunk order.
fn read_all(f: &Context, out: &mut Vec<Vec<u8>>) {
    for p in 0..f.get_count() {
        let Ok(st) = f.get_storage(p) else { continue };
        if st == Storage::SCANLINE || st == Storage::DEEP_SCANLINE {
            let (Ok(dw), Ok(lpc)) = (f.get_data_window(p), f.get_scanlines_per_chunk(p)) else {
                continue;
            };
            let mut y = i64::from(dw.min.y);
            while y <= i64::from(dw.max.y) {
                match f.read_scanline_chunk_info(p, y as i32) {
                    Ok(ci) => read_chunk(f, p, &ci, out),
                    Err(e) => out.push(
                        format!("line {p} {} {}", y as i32, get_error_code_as_string(e.code))
                            .into_bytes(),
                    ),
                }
                y += i64::from(lpc);
            }
        } else {
            let Ok((_, _, mode, _)) = f.get_tile_descriptor(p) else {
                continue;
            };
            let Ok((nlx, nly)) = f.get_tile_levels(p) else {
                continue;
            };
            let levels: Vec<(i32, i32)> = if mode == TileLevelMode::RIPMAP_LEVELS {
                (0..nly)
                    .flat_map(|ly| (0..nlx).map(move |lx| (lx, ly)))
                    .collect()
            } else {
                (0..nlx).map(|l| (l, l)).collect()
            };
            for (lx, ly) in levels {
                let Ok((cx, cy)) = f.get_tile_counts(p, lx, ly) else {
                    continue;
                };
                for ty in 0..cy {
                    for tx in 0..cx {
                        match f.read_tile_chunk_info(p, tx, ty, lx, ly) {
                            Ok(ci) => read_chunk(f, p, &ci, out),
                            Err(e) => out.push(
                                format!(
                                    "tile {p} {tx} {ty} {lx} {ly} {}",
                                    get_error_code_as_string(e.code)
                                )
                                .into_bytes(),
                            ),
                        }
                    }
                }
            }
        }
    }
}

/// The port's `core_harness info`: `print` the verbose context info, `read` every chunk.
fn port_info(path: &str, flags: i32, print: bool, read: bool) -> Info {
    let (init, msgs) = capture(flags);
    // OpenJPH prints its errors itself, on the same stderr as Core's handler in the harness
    openexr_openjph::set_error_stream(openexr_openjph::ErrorStream::Writer(Box::new(Sink(
        Arc::clone(&msgs),
    ))));
    let mut out = Vec::new();
    let rv = match Context::start_read(Some(path), Some(init)) {
        Ok(f) => {
            if print {
                out.extend(lines(&f.context_info(true)));
            }
            if read {
                read_all(&f, &mut out);
            }
            let _ = f.finish();
            Ok(())
        }
        Err(e) => Err(e),
    };
    out.push(format!("result {}", code(&rv)).into_bytes());
    let err = lines(&msgs.lock().unwrap());
    Info { out, err }
}

/// A writer into a context's messages, for OpenJPH's error stream.
struct Sink(Messages);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The deviation that makes the port's line `port` differ from Core's line `core`, when it is
/// one the owner waived (`docs/waivers.md`), and the difference is exactly the deviation's.
fn waived_line(core: &[u8], port: &[u8]) -> Option<&'static str> {
    let core = std::str::from_utf8(core).ok()?;
    let port = std::str::from_utf8(port).ok()?;
    // D-4 (W-4): exr_print_context_info prints bytes and deepImageState attributes as unknown
    // types; the port prints their values
    for t in ["bytes", "deepImageState"] {
        let unknown = format!("{t} <ERROR Unknown type '{t}'>");
        if let Some(prefix) = core.strip_suffix(&unknown)
            && port
                .strip_prefix(prefix)
                .is_some_and(|v| v.starts_with(&format!("{t} ")))
        {
            return Some("D-4");
        }
    }
    // D-6 (W-6): "Preparing to scanline" and "Preparing read scanline" corrected
    for garbled in ["Preparing to scanline ", "Preparing read scanline "] {
        if core.contains(garbled)
            && port == core.replacen(garbled, "Preparing to read scanline ", 1)
        {
            return Some("D-6");
        }
    }
    None
}

/// Whether the port's lines are Core's, every difference one of [`waived_line`]'s; and the
/// deviations found.
fn same_but_waived(core: &[Vec<u8>], port: &[Vec<u8>]) -> (bool, Vec<&'static str>) {
    if core.len() != port.len() {
        return (false, Vec::new());
    }
    let mut found = Vec::new();
    for (c, p) in core.iter().zip(port) {
        if c != p {
            match waived_line(c, p) {
                Some(d) => found.push(d),
                None => return (false, found),
            }
        }
    }
    (true, found)
}

fn show(v: &[Vec<u8>]) -> String {
    v.iter()
        .map(|l| String::from_utf8_lossy(l).into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Heavy tier: every image opens and reads as Core 3.5.3 opens and reads it (the harness),
/// chunk by chunk, with the same messages; and prints the same context info.
#[test]
fn images_read_and_print_as_core_does() {
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_harness_read_{}",
        std::process::id()
    ));
    let files = read_set(&scratch);
    let (mut compared, mut differing) = (0, Vec::new());
    let mut waived: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for file in &files {
        let path = file.to_str().expect("utf-8 path");
        for (args, flags, print, read) in [
            (
                &["--silent", "--no-print", "--read"][..],
                EXR_CONTEXT_FLAG_SILENT_HEADER_PARSE,
                false,
                true,
            ),
            (&["--verbose"][..], 0, true, false),
        ] {
            let h = harness_info(path, args);
            let m = port_info(path, flags, print, read);
            compared += 1;
            let (out_same, out_waived) = same_but_waived(&h.out, &m.out);
            let (err_same, err_waived) = same_but_waived(&h.err, &m.err);
            for w in out_waived.into_iter().chain(err_waived) {
                *waived.entry(w).or_insert(0) += 1;
            }
            if !out_same || !err_same {
                differing.push(format!(
                    "{path} {args:?}:\n-- harness out\n{}\n-- port out\n{}\n-- harness err\n{}\n-- port err\n{}",
                    show(&h.out),
                    show(&m.out),
                    show(&h.err),
                    show(&m.err)
                ));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    for d in differing.iter().take(20) {
        eprintln!("{d}\n");
    }
    eprintln!(
        "files: {}, runs compared: {compared}, lines left out as deviations: {waived:?}, differing: {}",
        files.len(),
        differing.len()
    );
    assert!(differing.is_empty());
}

/// Each part's header bytes (everything before the chunk tables) and chunks by index: a
/// parse of the file of its own, independent of the port's reader.
fn file_parts(data: &[u8]) -> (Vec<u8>, Vec<Vec<Vec<u8>>>) {
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let i32_at = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(data[o..o + 8].try_into().unwrap());
    let flags = u32_at(4);
    let multipart = flags & 0x1000 != 0;
    let mut pos = 8;
    let mut parts: Vec<(bool, Option<i32>)> = Vec::new();
    loop {
        let (mut tiled, mut count, mut any) = (false, None, false);
        loop {
            let end = pos + data[pos..].iter().position(|&b| b == 0).unwrap();
            let name = &data[pos..end];
            pos = end + 1;
            if name.is_empty() {
                break;
            }
            any = true;
            let tend = pos + data[pos..].iter().position(|&b| b == 0).unwrap();
            pos = tend + 1;
            let size = i32_at(pos) as usize;
            pos += 4;
            if name == b"tiles" {
                tiled = true;
            }
            if name == b"chunkCount" {
                count = Some(i32_at(pos));
            }
            pos += size;
        }
        if !any {
            break;
        }
        parts.push((tiled, count));
        if !multipart {
            break;
        }
    }
    let header = data[..pos].to_vec();
    let mut counts = Vec::new();
    if parts.len() > 1 {
        counts = parts.iter().map(|p| p.1.unwrap() as usize).collect();
    } else {
        // the table ends where its lowest chunk starts
        let (mut n, mut lowest) = (0usize, data.len() as u64);
        while ((pos + 8 * n) as u64) < lowest {
            lowest = lowest.min(u64_at(pos + 8 * n));
            n += 1;
        }
        counts.push(n);
    }
    let mut out = Vec::new();
    let mut t = pos;
    for (part, n) in parts.iter().zip(counts) {
        let lead = if multipart { 4 } else { 0 } + if part.0 { 16 } else { 4 };
        let mut chunks = Vec::new();
        for i in 0..n {
            let o = u64_at(t + 8 * i) as usize;
            let size = i32_at(o + lead) as usize;
            chunks.push(data[o..o + lead + 4 + size].to_vec());
        }
        t += 8 * n;
        out.push(chunks);
    }
    (header, out)
}

/// The levels a rewrite sets, as the harness's options.
#[derive(Clone, Copy, Debug)]
enum Level {
    Default,
    Zip(i32),
    Zstd(i32),
    Dwa(f32),
}

/// `floor (x / s)`, and how many of the lines `[a, b)` a channel sampled every `s` holds.
fn sampled(a: i64, b: i64, s: i64) -> i64 {
    let fd = |x: i64| if x >= 0 { x / s } else { -((-x + s - 1) / s) };
    fd(b - 1) - fd(a - 1)
}

/// The port's `core_harness recompress`, chunks in the port's write order (D-14).
fn port_recompress(
    src: &Context,
    out: &str,
    comp: Compression,
    level: Level,
    init: Initializer,
) -> Result<(), Error> {
    let mut dst = Context::start_write(Some(out), DefaultWriteMode::WriteFileDirectly, Some(init))?;
    for p in 0..src.get_count() {
        let name = src
            .get_name(p)
            .ok()
            .map(|n| String::from_utf8_lossy(n).into_owned());
        let q = dst.add_part(name.as_deref(), src.get_storage(p)?)?;
        dst.copy_unset_attributes(q, src, p)?;
        dst.set_compression(q, comp)?;
        match level {
            Level::Default => {}
            Level::Zip(l) => dst.set_zip_compression_level(q, l)?,
            Level::Zstd(l) => dst.set_zstd_compression_level(q, l)?,
            Level::Dwa(l) => dst.set_dwa_compression_level(q, l)?,
        }
    }
    dst.write_header()?;

    for p in 0..src.get_count() {
        let decreasing = src.get_lineorder(p)? == LineOrder::DECREASING_Y;
        if src.get_storage(p)? == Storage::SCANLINE {
            let dw = src.get_data_window(p)?;
            let chans: Vec<(i64, i64, i64)> = src
                .get_channels(p)?
                .entries()
                .iter()
                .map(|e| {
                    let w = sampled(
                        i64::from(dw.min.x),
                        i64::from(dw.max.x) + 1,
                        i64::from(e.x_sampling),
                    );
                    let bpe = if e.pixel_type == PixelType::HALF {
                        2
                    } else {
                        4
                    };
                    (w, bpe, i64::from(e.y_sampling))
                })
                .collect();
            let mut planes: Vec<Vec<u8>> = chans
                .iter()
                .map(|&(w, bpe, ys)| {
                    vec![
                        0u8;
                        (w * bpe * sampled(i64::from(dw.min.y), i64::from(dw.max.y) + 1, ys))
                            as usize
                    ]
                })
                .collect();
            let lpc_in = src.get_scanlines_per_chunk(p)?;
            let mut y = dw.min.y;
            while y <= dw.max.y {
                let ci = src.read_scanline_chunk_info(p, y)?;
                let mut dec = src.decoding_initialize(p, &ci)?;
                for (c, ch) in dec.channels.iter_mut().enumerate() {
                    let (w, bpe, ys) = chans[c];
                    let row = sampled(i64::from(dw.min.y), i64::from(ci.start_y), ys);
                    ch.data = Some(BufferRef {
                        buffer: c,
                        offset: (row * w * bpe) as usize,
                    });
                    ch.user_pixel_stride = i32::from(ch.bytes_per_element);
                    ch.user_line_stride = (w * bpe) as i32;
                }
                src.decoding_choose_default_routines(p, &mut dec)?;
                let mut outs: Vec<&mut [u8]> = planes.iter_mut().map(|b| &mut b[..]).collect();
                src.decoding_run(p, &mut dec, &mut outs)?;
                src.decoding_destroy(&mut dec)?;
                y += lpc_in;
            }
            let lpc_out = dst.get_scanlines_per_chunk(p)?;
            let mut starts = Vec::new();
            let mut y = dw.min.y;
            while y <= dw.max.y {
                starts.push(y);
                y += lpc_out;
            }
            if decreasing {
                starts.reverse(); // D-14: the C++ library's order
            }
            for y in starts {
                let co = dst.write_scanline_chunk_info(p, y)?;
                let mut enc = dst.encoding_initialize(p, &co)?;
                for (c, ch) in enc.channels.iter_mut().enumerate() {
                    let (w, bpe, ys) = chans[c];
                    let row = sampled(i64::from(dw.min.y), i64::from(co.start_y), ys);
                    ch.data = Some(BufferRef {
                        buffer: c,
                        offset: (row * w * bpe) as usize,
                    });
                    ch.user_pixel_stride = i32::from(ch.bytes_per_element);
                    ch.user_line_stride = (w * bpe) as i32;
                }
                dst.encoding_choose_default_routines(p, &mut enc)?;
                let ins: Vec<&[u8]> = planes.iter().map(|b| &b[..]).collect();
                dst.encoding_run(p, &mut enc, &ins)?;
                dst.encoding_destroy(&mut enc)?;
            }
        } else {
            let (nlx, nly) = src.get_tile_levels(p)?;
            let levels: Vec<(i32, i32)> =
                if src.get_tile_descriptor(p)?.2 == TileLevelMode::RIPMAP_LEVELS {
                    (0..nly)
                        .flat_map(|ly| (0..nlx).map(move |lx| (lx, ly)))
                        .collect()
                } else {
                    (0..nlx).map(|l| (l, l)).collect()
                };
            for (lx, ly) in levels {
                let (cx, cy) = src.get_tile_counts(p, lx, ly)?;
                let mut rows: Vec<i32> = (0..cy).collect();
                if decreasing {
                    rows.reverse(); // D-14: each level from its top row of tiles down
                }
                for ty in rows {
                    for tx in 0..cx {
                        let ci = src.read_tile_chunk_info(p, tx, ty, lx, ly)?;
                        let mut dec = src.decoding_initialize(p, &ci)?;
                        let mut bufs = Vec::new();
                        for (c, ch) in dec.channels.iter_mut().enumerate() {
                            let bpe = i32::from(ch.bytes_per_element);
                            ch.user_pixel_stride = bpe;
                            ch.user_line_stride = ch.width * bpe;
                            ch.data = Some(BufferRef {
                                buffer: c,
                                offset: 0,
                            });
                            bufs.push(vec![
                                0u8;
                                (ch.width.max(0) * ch.height.max(0) * bpe) as usize
                            ]);
                        }
                        src.decoding_choose_default_routines(p, &mut dec)?;
                        let mut outs: Vec<&mut [u8]> =
                            bufs.iter_mut().map(|b| &mut b[..]).collect();
                        src.decoding_run(p, &mut dec, &mut outs)?;
                        src.decoding_destroy(&mut dec)?;

                        let co = dst.write_tile_chunk_info(p, tx, ty, lx, ly)?;
                        let mut enc = dst.encoding_initialize(p, &co)?;
                        for (c, ch) in enc.channels.iter_mut().enumerate() {
                            let bpe = i32::from(ch.bytes_per_element);
                            ch.user_pixel_stride = bpe;
                            ch.user_line_stride = ch.width * bpe;
                            ch.data = Some(BufferRef {
                                buffer: c,
                                offset: 0,
                            });
                        }
                        dst.encoding_choose_default_routines(p, &mut enc)?;
                        let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
                        dst.encoding_run(p, &mut enc, &ins)?;
                        dst.encoding_destroy(&mut enc)?;
                    }
                }
            }
        }
    }
    dst.finish()
}

/// Whether `chunk` (as Core wrote it) is a tile of a level above 0 at the level's right or
/// bottom edge: the tiles whose size upstream's `exr_write_tile_chunk_info` clips against the
/// data window and the port against the level (D-17, I-21).
fn level_edge_tile(src: &Context, p: i32, chunk: &[u8], multipart: bool) -> bool {
    if src.get_storage(p).ok() != Some(Storage::TILED) {
        return false;
    }
    let o = if multipart { 4 } else { 0 };
    let at = |k: usize| i32::from_le_bytes(chunk[o + 4 * k..o + 4 * k + 4].try_into().unwrap());
    let (tx, ty, lx, ly) = (at(0), at(1), at(2), at(3));
    if lx == 0 && ly == 0 {
        return false;
    }
    let (tw, th, _, _) = src.get_tile_descriptor(p).unwrap();
    let Ok((lw, lh)) = src.get_level_sizes(p, lx, ly) else {
        return false;
    };
    i64::from(tx + 1) * i64::from(tw) > i64::from(lw)
        || i64::from(ty + 1) * i64::from(th) > i64::from(lh)
}

/// Whether part `p` of `src` has a half channel.
fn has_half(src: &Context, p: usize) -> bool {
    src.get_channels(p as i32)
        .unwrap()
        .entries()
        .iter()
        .any(|e| e.pixel_type == PixelType::HALF)
}

/// Whether the port opens `path` without a message, and it holds no deep part: a source both
/// can rewrite.
fn clean_source(path: &Path) -> bool {
    let (init, msgs) = capture(0);
    let Ok(f) = Context::start_read(Some(path.to_str().unwrap()), Some(init)) else {
        return false;
    };
    let flat = (0..f.get_count())
        .all(|p| matches!(f.get_storage(p), Ok(Storage::SCANLINE) | Ok(Storage::TILED)));
    // and every chunk decodes
    let mut out = Vec::new();
    if flat {
        read_all(&f, &mut out);
    }
    let decodes = out.iter().all(|l| {
        let l = String::from_utf8_lossy(l);
        l.starts_with("chunk ") && l.contains(" EXR_ERR_SUCCESS")
    });
    flat && decodes && msgs.lock().unwrap().is_empty()
}

/// Heavy tier: files Core writes alone come out as Core 3.5.3 writes them (the harness): the
/// header byte for byte, each chunk by index, for every layout, compression and level asked.
#[test]
fn files_written_by_core_alone_as_core_writes_them() {
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_harness_write_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    write_images(&scratch);
    let mut sources: Vec<PathBuf> = std::fs::read_dir(&scratch)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with("_NO_COMPRESSION.exr"))
        .collect();
    sources.sort();
    // multipart files (oracle/write_multipart.py), and upstream's images both can rewrite
    let multipart = scratch.join("multipart");
    let status = oracle_python()
        .arg(root().join("oracle/write_multipart.py"))
        .arg(&multipart)
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "write_multipart.py failed");
    exr_files(&multipart, &mut sources);
    let mut upstream = Vec::new();
    exr_files(&root().join("upstream/openexr/src/test"), &mut upstream);
    sources.extend(upstream.into_iter().filter(|p| clean_source(p)));

    let mut runs: Vec<(Compression, Level)> =
        COMPRESSIONS.iter().map(|c| (c.0, Level::Default)).collect();
    for l in 0..=9 {
        runs.push((Compression::ZIP, Level::Zip(l)));
        runs.push((Compression::ZIPS, Level::Zip(l)));
    }
    for l in -1..=22 {
        runs.push((Compression::ZSTD, Level::Zstd(l)));
    }
    for l in [0.0, 1.0, 45.0, 100.0, 1000.0] {
        runs.push((Compression::DWAA, Level::Dwa(l)));
        runs.push((Compression::DWAB, Level::Dwa(l)));
    }

    let (mut compared, mut unported, mut deviations, mut differing) = (0, 0, 0, Vec::new());
    // both write to the same name, which Core's messages carry
    let out = scratch.join("rewrite.exr");
    for src_path in &sources {
        let layout = src_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace("_NO_COMPRESSION.exr", "");
        let src =
            Context::start_read(Some(src_path.to_str().unwrap()), None).expect("the port reads it");
        for &(comp, level) in &runs {
            let mut cmd = Command::new(harness());
            cmd.arg("recompress")
                .arg(src_path)
                .arg(&out)
                .arg(comp.0.to_string());
            match level {
                Level::Default => {}
                Level::Zip(l) => {
                    cmd.args(["--zip", &l.to_string()]);
                }
                Level::Zstd(l) => {
                    cmd.args(["--zstd", &l.to_string()]);
                }
                Level::Dwa(l) => {
                    cmd.args(["--dwa", &l.to_string()]);
                }
            }
            let h = cmd.output().expect("the harness runs");
            let what = format!("{layout} {comp:?} {level:?}");
            let a = if h.status.success() {
                std::fs::read(&out).unwrap()
            } else {
                Vec::new()
            };
            let _ = std::fs::remove_file(&out);
            let (init, msgs) = capture(0);
            let port = port_recompress(&src, out.to_str().unwrap(), comp, level, init);
            let port_msgs = lines(&msgs.lock().unwrap());
            if !h.status.success() {
                // a refusal: the same code, and the same messages from Core
                let herr = lines(&h.stderr);
                let (last, core_msgs) = herr.split_last().expect("the harness says why");
                let hcode = String::from_utf8_lossy(last)
                    .rsplit(": ")
                    .next()
                    .unwrap()
                    .to_string();
                if core_msgs
                    .iter()
                    .any(|m| m.ends_with(b"Invalid reference to bytes attribute to save."))
                {
                    deviations += 1; // D-10 (I-14): the port writes the bytes attribute
                    continue;
                }
                match &port {
                    Err(e)
                        if get_error_code_as_string(e.code) == hcode && port_msgs == core_msgs =>
                    {
                        compared += 1
                    }
                    _ => differing.push(format!(
                        "{what}: the harness refused ({hcode}: {}), the port: {} {}",
                        show(core_msgs),
                        code(&port),
                        show(&port_msgs)
                    )),
                }
                continue;
            }
            if let Err(e) = &port {
                if matches!(level, Level::Zstd(_))
                    && e.code == openexr_core::errors::ErrorCode::CompressionFailed
                {
                    unported += 1; // zstd's strategies not yet ported (docs/deferred.md)
                    continue;
                }
                differing.push(format!(
                    "{what}: the port failed: {:?} {}",
                    e.code,
                    show(&port_msgs)
                ));
                continue;
            }
            let b = std::fs::read(&out).unwrap();
            compared += 1;
            let (ha, ca) = file_parts(&a);
            let (hb, cb) = file_parts(&b);
            let b44 = matches!(comp, Compression::B44 | Compression::B44A);
            let decreasing = (0..src.get_count())
                .any(|p| src.get_lineorder(p).unwrap() == LineOrder::DECREASING_Y);
            if ha != hb {
                differing.push(format!("{what}: the headers differ"));
            } else if ca.len() != cb.len() {
                differing.push(format!("{what}: the part counts differ"));
            } else if ca != cb {
                for (p, (x, y)) in ca.iter().zip(&cb).enumerate() {
                    if x == y {
                        continue;
                    }
                    if b44 && !has_half(&src, p) {
                        deviations += 1; // D-18 (I-23): the port keeps the packed bytes
                        continue;
                    }
                    let multipart = src.get_count() > 1;
                    let mut bad = Vec::new();
                    for (i, (cx, cy)) in x.iter().zip(y).enumerate() {
                        if cx == cy {
                            continue;
                        }
                        if level_edge_tile(&src, p as i32, cx, multipart) {
                            deviations += 1; // D-17 (I-21): the port clips against the level
                        } else {
                            bad.push(i);
                        }
                    }
                    if !bad.is_empty() {
                        differing.push(format!("{what}: part {p}: chunks differ: {bad:?}"));
                    }
                }
            } else if a != b && !decreasing {
                // only a decreasing-Y part may store the same chunks in another order (D-14)
                differing.push(format!("{what}: the same chunks, stored differently"));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    for d in differing.iter().take(40) {
        eprintln!("{d}");
    }
    eprintln!(
        "rewrites compared: {compared}; the port's deviations: {deviations}; zstd levels not yet ported: {unported}; differing: {}",
        differing.len()
    );
    assert!(compared > 0);
    assert!(differing.is_empty());
}
