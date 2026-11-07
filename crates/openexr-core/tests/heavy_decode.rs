// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8): images decode bit for bit as the official wheel decodes them, for
//! every codec the port decodes so far ([`DECODES`]).
//!
//! The images are upstream's test directory and small images the wheel writes in many channel
//! layouts (`oracle/write_images.py`). Each is read by the oracle (`oracle/dump_pixels.py`)
//! and by the port. Each part of scanline or tiled storage with one of [`DECODES`]'
//! compressions is decoded whole (level 0 of a tiled part, the level the wheel reads), twice:
//! into one planar buffer per channel, and, when its channels are the same size and not
//! subsampled, into one interleaved buffer in each channel order; and planar again with a gap
//! after every pixel. Each is compared byte for byte with the wheel's pixels. The two layouts reach the direct read and the unpack routines (planar,
//! interleaved, generic). An image the wheel reads but the port refuses fails the check; so
//! does a part whose pixels one of them decodes and the other refuses.

use std::path::{Path, PathBuf};
use std::process::Command;

use openexr_core::coding::BufferRef;
use openexr_core::context::{Context, Error};
use openexr_core::openexr_attr::{Compression, Storage};

/// The compressions the port decodes, with the wheel's names for them.
const DECODES: [(Compression, &str); 5] = [
    (Compression::NONE, "NO_COMPRESSION"),
    (Compression::RLE, "RLE_COMPRESSION"),
    (Compression::ZIPS, "ZIPS_COMPRESSION"),
    (Compression::ZIP, "ZIP_COMPRESSION"),
    (Compression::PIZ, "PIZ_COMPRESSION"),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn oracle_script(script: &str) -> Command {
    let root = root();
    let mut c = Command::new("uv");
    c.args(["run", "--project"])
        .arg(root.join("oracle"))
        .args(["python", "-I"])
        .arg(root.join(script));
    c
}

/// A channel as the wheel decoded it.
struct OracleChannel {
    part: i32,
    name: Vec<u8>,
    data: Vec<u8>,
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// The wheel's channels of `image`, or its refusal.
fn oracle(image: &Path, out: &Path) -> Result<Vec<OracleChannel>, String> {
    let status = oracle_script("oracle/dump_pixels.py")
        .arg(image)
        .arg(out)
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "oracle failed on {}", image.display());
    let manifest = std::fs::read_to_string(out.join("manifest.tsv")).expect("manifest");
    let mut chans = Vec::new();
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] == "error" {
            return Err(cols.get(1).unwrap_or(&"").to_string());
        }
        chans.push(OracleChannel {
            part: cols[0].parse().expect("part"),
            name: unhex(cols[2]),
            data: std::fs::read(out.join(cols[8])).expect("channel file"),
        });
    }
    Ok(chans)
}

/// How many of the coordinates `lo..=hi` are multiples of `s`: a channel's samples.
fn samples(lo: i32, hi: i32, s: i32) -> usize {
    (lo..=hi).filter(|v| v % s == 0).count()
}

/// A channel of a part: its name, bytes per element, sampling and sampled size.
struct Chan {
    name: Vec<u8>,
    bpe: usize,
    w: usize,
    h: usize,
}

/// A chunk of the part and the first row and column (in samples, per channel) it starts at.
struct ChunkAt {
    cinfo: openexr_core::chunk_info::ChunkInfo,
    x0: Vec<usize>,
    y0: Vec<usize>,
}

/// The port's decode of part `p`, planar and (when possible) interleaved: each channel's
/// name and pixels. `None` for a part this phase does not decode.
/// Each channel's name and pixels.
type Planes = Vec<(Vec<u8>, Vec<u8>)>;

fn decode_part(f: &Context, p: i32) -> Result<Option<(Planes, Vec<Planes>)>, Error> {
    let storage = f.get_storage(p)?;
    let comp = f.get_compression(p)?;
    if !DECODES.iter().any(|(c, _)| *c == comp)
        || (storage != Storage::SCANLINE && storage != Storage::TILED)
    {
        return Ok(None);
    }
    let dw = f.get_data_window(p)?;

    let probe = if storage == Storage::SCANLINE {
        f.read_scanline_chunk_info(p, dw.min.y)?
    } else {
        f.read_tile_chunk_info(p, 0, 0, 0, 0)?
    };
    let first = f.decoding_initialize(p, &probe)?;
    let chans: Vec<Chan> = first
        .channels
        .iter()
        .map(|c| Chan {
            name: c.channel_name.clone(),
            bpe: c.bytes_per_element as usize,
            w: samples(dw.min.x, dw.max.x, c.x_samples),
            h: samples(dw.min.y, dw.max.y, c.y_samples),
        })
        .collect();
    let sampling: Vec<(i32, i32)> = first
        .channels
        .iter()
        .map(|c| (c.x_samples, c.y_samples))
        .collect();
    drop(first);

    let mut chunks = Vec::new();
    if storage == Storage::SCANLINE {
        let lpc = f.get_scanlines_per_chunk(p)?;
        let mut y = dw.min.y;
        while y <= dw.max.y {
            let cinfo = f.read_scanline_chunk_info(p, y)?;
            let y0 = sampling
                .iter()
                .map(|&(_, ys)| samples(dw.min.y, cinfo.start_y - 1, ys))
                .collect();
            chunks.push(ChunkAt {
                cinfo,
                x0: vec![0; chans.len()],
                y0,
            });
            y += lpc;
        }
    } else {
        let (tx_n, ty_n) = f.get_tile_counts(p, 0, 0)?;
        let (tsx, tsy) = f.get_tile_sizes(p, 0, 0)?;
        for ty in 0..ty_n {
            for tx in 0..tx_n {
                let cinfo = f.read_tile_chunk_info(p, tx, ty, 0, 0)?;
                let n = chans.len();
                chunks.push(ChunkAt {
                    cinfo,
                    x0: vec![(tx * tsx) as usize; n],
                    y0: vec![(ty * tsy) as usize; n],
                });
            }
        }
    }

    // planar: buffer c holds channel c
    let mut planar: Vec<Vec<u8>> = chans.iter().map(|c| vec![0u8; c.w * c.h * c.bpe]).collect();
    for ch in &chunks {
        let mut d = f.decoding_initialize(p, &ch.cinfo)?;
        for (c, out) in d.channels.iter_mut().enumerate() {
            let k = &chans[c];
            out.data = Some(BufferRef {
                buffer: c,
                offset: (ch.y0[c] * k.w + ch.x0[c]) * k.bpe,
            });
            out.user_pixel_stride = k.bpe as i32;
            out.user_line_stride = (k.w * k.bpe) as i32;
        }
        f.decoding_choose_default_routines(p, &mut d)?;
        let mut outs: Vec<&mut [u8]> = planar.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(p, &mut d, &mut outs)?;
    }
    let planar_out = chans.iter().map(|c| c.name.clone()).zip(planar).collect();

    // interleaved: one buffer, every channel's pixels side by side
    let same = chans
        .iter()
        .all(|c| c.bpe == chans[0].bpe && c.w == chans[0].w && c.h == chans[0].h)
        && sampling.iter().all(|&s| s == (1, 1));
    // planar with a gap after every pixel: each channel at its own pixel stride
    let mut spaced: Vec<Vec<u8>> = chans
        .iter()
        .map(|c| vec![0u8; c.w * c.h * c.bpe * 2])
        .collect();
    for ch in &chunks {
        let mut d = f.decoding_initialize(p, &ch.cinfo)?;
        for (c, out) in d.channels.iter_mut().enumerate() {
            let k = &chans[c];
            out.data = Some(BufferRef {
                buffer: c,
                offset: (ch.y0[c] * k.w + ch.x0[c]) * k.bpe * 2,
            });
            out.user_pixel_stride = (k.bpe * 2) as i32;
            out.user_line_stride = (k.w * k.bpe * 2) as i32;
        }
        f.decoding_choose_default_routines(p, &mut d)?;
        let mut outs: Vec<&mut [u8]> = spaced.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(p, &mut d, &mut outs)?;
    }
    let spaced_out: Planes = chans
        .iter()
        .zip(spaced)
        .map(|(k, b)| {
            let plane = b
                .chunks(k.bpe * 2)
                .flat_map(|px| px[..k.bpe].to_vec())
                .collect();
            (k.name.clone(), plane)
        })
        .collect();

    let mut interleaved_out = Vec::new();
    for rev in [false, true] {
        if !same {
            break;
        }
        let (n, bpe, w, h) = (chans.len(), chans[0].bpe, chans[0].w, chans[0].h);
        // the slot of channel c in a pixel: c, or n - 1 - c in the reversed order
        let slot = |c: usize| if rev { n - 1 - c } else { c };
        let mut buf = [vec![0u8; w * h * n * bpe]];
        for ch in &chunks {
            let mut d = f.decoding_initialize(p, &ch.cinfo)?;
            for (c, out) in d.channels.iter_mut().enumerate() {
                out.data = Some(BufferRef {
                    buffer: 0,
                    offset: ((ch.y0[c] * w + ch.x0[c]) * n + slot(c)) * bpe,
                });
                out.user_pixel_stride = (n * bpe) as i32;
                out.user_line_stride = (w * n * bpe) as i32;
            }
            f.decoding_choose_default_routines(p, &mut d)?;
            let mut outs: Vec<&mut [u8]> = buf.iter_mut().map(Vec::as_mut_slice).collect();
            f.decoding_run(p, &mut d, &mut outs)?;
        }
        interleaved_out.push(
            chans
                .iter()
                .enumerate()
                .map(|(c, k)| {
                    let mut plane = Vec::with_capacity(w * h * bpe);
                    for px in 0..w * h {
                        let at = (px * n + slot(c)) * bpe;
                        plane.extend_from_slice(&buf[0][at..at + bpe]);
                    }
                    (k.name.clone(), plane)
                })
                .collect(),
        );
    }
    interleaved_out.insert(0, spaced_out);
    Ok(Some((planar_out, interleaved_out)))
}

fn images(scratch: &Path) -> Vec<PathBuf> {
    let dir = root().join("upstream/openexr/src/test/OpenEXRTest");
    let mut images: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("test images")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "exr"))
        .collect();
    let written = scratch.join("written");
    let status = oracle_script("oracle/write_images.py")
        .arg(&written)
        .args(DECODES.iter().map(|(_, n)| *n))
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "write_images.py failed");
    images.extend(
        std::fs::read_dir(&written)
            .expect("written")
            .filter_map(|e| e.ok().map(|e| e.path())),
    );
    images.sort();
    images
}

#[test]
fn images_decode_as_the_wheel_does() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_decode_{}", std::process::id()));
    let (mut parts_checked, mut layouts_checked, mut parts_skipped, mut parts_refused) =
        (0, 0, 0, 0);
    for (n, image) in images(&scratch).iter().enumerate() {
        let out = scratch.join(n.to_string());
        let wheel = oracle(image, &out);
        let path = image.to_str().expect("utf-8 path");
        let port = Context::start_read(Some(path), None);
        let (wheel, f) = match (wheel, port) {
            (Err(_), _) => continue, // the wheel refuses it: acceptance is checked elsewhere
            (Ok(_), Err(e)) => panic!(
                "{}: the wheel reads it, the port refuses it: {e}",
                image.display()
            ),
            (Ok(w), Ok(f)) => (w, f),
        };
        for p in 0..f.get_count() {
            let wheel_read_it = wheel.iter().any(|c| c.part == p);
            let (planar, interleaved) = match decode_part(&f, p) {
                Ok(None) => {
                    parts_skipped += 1;
                    continue;
                }
                // the wheel could not read this part's pixels either
                Err(_) if !wheel_read_it => {
                    parts_refused += 1;
                    continue;
                }
                Err(e) => panic!(
                    "{}: part {p}: the wheel reads it, the port fails: {e}",
                    image.display()
                ),
                Ok(Some(_)) if !wheel_read_it => {
                    panic!(
                        "{}: part {p}: the port decodes it, the wheel could not",
                        image.display()
                    )
                }
                Ok(Some(d)) => d,
            };
            let names = ["planar, spaced", "interleaved", "interleaved, reversed"];
            let layouts =
                std::iter::once(("planar", planar)).chain(names.into_iter().zip(interleaved));
            for (layout, chans) in layouts {
                for (name, data) in chans {
                    let w = wheel
                        .iter()
                        .find(|c| c.part == p && c.name == name)
                        .unwrap_or_else(|| {
                            panic!(
                                "{}: part {p} channel {name:?} not in the wheel's",
                                image.display()
                            )
                        });
                    assert!(
                        w.data == data,
                        "{}: part {p} channel {} ({layout}): pixels differ",
                        image.display(),
                        String::from_utf8_lossy(&name)
                    );
                }
                layouts_checked += 1;
            }
            parts_checked += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "parts compared: {parts_checked} ({layouts_checked} layouts); refused by both: {parts_refused}; parts for later phases: {parts_skipped}"
    );
    assert!(parts_checked > 0);
}

/// A write context into memory with `src`'s parts (copied, with a legacy header: the C++
/// library's attribute order), its header written; and the memory.
fn header_copy(src: &Context) -> Result<(Context, Memory), Error> {
    use openexr_core::context::{
        DefaultWriteMode, EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER, Initializer,
    };
    use std::sync::{Arc, Mutex};
    let mem = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&mem);
    let cinit = Initializer {
        write_fn: Some(Box::new(move |_, buf, off| {
            let mut m = sink.lock().unwrap();
            let end = off as usize + buf.len();
            if m.len() < end {
                m.resize(end, 0);
            }
            m[off as usize..end].copy_from_slice(buf);
            buf.len() as i64
        })),
        flags: EXR_CONTEXT_FLAG_WRITE_LEGACY_HEADER,
        ..Initializer::default()
    };
    let mut dst = Context::start_write(
        Some("<memory>"),
        DefaultWriteMode::WriteFileDirectly,
        Some(cinit),
    )?;
    for p in 0..src.get_count() {
        let name = src
            .get_name(p)
            .ok()
            .map(|n| String::from_utf8_lossy(n).into_owned());
        let q = dst.add_part(name.as_deref(), src.get_storage(p)?)?;
        dst.copy_unset_attributes(q, src, p)?;
    }
    dst.write_header()?;
    Ok((dst, mem))
}

/// The memory a [`header_copy`] context writes to.
type Memory = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// The header the port writes for `src`'s parts ([`header_copy`]).
fn rewrite_header(src: &Context) -> Result<Vec<u8>, Error> {
    let (_, mem) = header_copy(src)?;
    let bytes = mem.lock().unwrap().clone();
    Ok(bytes)
}

/// A single-part header without its `type` attribute: the record (name, type name, size,
/// value) is cut out, every other byte kept. The C++ library always writes `type`; a legacy
/// Core header leaves it out of single-part files (write_header.c:719-728 @ v3.5.3).
fn without_type_attr(header: &[u8]) -> Vec<u8> {
    let mut out = header[..8].to_vec();
    let mut at = 8;
    loop {
        let start = at;
        let name_end = at + header[at..].iter().position(|&b| b == 0).expect("name");
        if name_end == at {
            out.extend_from_slice(&header[at..]);
            return out;
        }
        let type_end = name_end
            + 1
            + header[name_end + 1..]
                .iter()
                .position(|&b| b == 0)
                .expect("type");
        let size =
            i32::from_le_bytes(header[type_end + 1..type_end + 5].try_into().unwrap()) as usize;
        at = type_end + 5 + size;
        if &header[start..name_end] != b"type" {
            out.extend_from_slice(&header[start..at]);
        }
    }
}

/// Heavy tier: the headers the wheel (the C++ library) writes come back byte for byte, but for
/// a single-part file's `type` attribute ([`without_type_attr`]), when the port writes them
/// from the attributes it read: every image written by
/// oracle/write_images.py, among them a header with an attribute of every type the binding
/// writes, and upstream's test images but the corrupt comp_invalid_unknown.exr and the
/// `*_with_*_type.exr` images, single-part files carrying a `type` attribute that a legacy
/// header leaves out.
#[test]
fn headers_rewrite_as_the_wheel_wrote_them() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_headers_{}", std::process::id()));
    let mut compared = 0;
    for image in images(&scratch) {
        let name = image.file_name().unwrap().to_string_lossy().into_owned();
        if name == "comp_invalid_unknown.exr" || name.contains("_with_") {
            continue;
        }
        let Ok(src) = Context::start_read(Some(image.to_str().unwrap()), None) else {
            continue;
        };
        let file = std::fs::read(&image).unwrap();
        let end = src.get_chunk_table_offset(0).unwrap() as usize;
        let mut file = file[..end].to_vec();
        if src.get_count() == 1 {
            file = without_type_attr(&file);
        }
        let end = file.len();
        let h = rewrite_header(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
        if h[..] != file[..end] {
            let at = h
                .iter()
                .zip(&file[..end])
                .position(|(a, b)| a != b)
                .unwrap_or(h.len().min(end));
            let lo = at.saturating_sub(24);
            panic!(
                "{name}: the header differs at byte {at} (port {} bytes, file {end})
 port: {:?}
 file: {:?}",
                h.len(),
                String::from_utf8_lossy(&h[lo..(at + 24).min(h.len())]),
                String::from_utf8_lossy(&file[lo..(at + 24).min(end)])
            );
        }
        compared += 1;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("headers compared: {compared}");
    assert!(compared > 0);
}

/// The compressions the port encodes.
const ENCODES: [Compression; 5] = [
    Compression::NONE,
    Compression::RLE,
    Compression::ZIPS,
    Compression::ZIP,
    Compression::PIZ,
];

/// A chunk of a file: its first line (or tile x, tile y and levels) and its information.
type FileChunk = (i32, i32, i32, i32, openexr_core::chunk_info::ChunkInfo);

/// Part `p`'s chunks, in the order the C++ library writes them: chunk order; for a
/// decreasing-Y part, from the bottom (scanlines), or each level from its top row of tiles
/// down (tiles). Each is its first line (or tile and levels) and its information.
fn chunks_in_write_order(src: &Context, p: i32) -> Result<Vec<FileChunk>, Error> {
    use openexr_core::openexr_attr::{LineOrder, TileLevelMode};
    let mut chunks = Vec::new();
    if src.get_storage(p)? == Storage::SCANLINE {
        let dw = src.get_data_window(p)?;
        let lpc = src.get_scanlines_per_chunk(p)?;
        let mut y = dw.min.y;
        while y <= dw.max.y {
            chunks.push((y, 0, 0, 0, src.read_scanline_chunk_info(p, y)?));
            y += lpc;
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
            for ty in 0..cy {
                for tx in 0..cx {
                    chunks.push((tx, ty, lx, ly, src.read_tile_chunk_info(p, tx, ty, lx, ly)?));
                }
            }
        }
    }
    chunks.sort_by_key(|c| c.4.idx);
    if src.get_storage(p)? == Storage::SCANLINE && src.get_lineorder(p)? == LineOrder::DECREASING_Y
    {
        chunks.reverse();
    } else if src.get_lineorder(p)? == LineOrder::DECREASING_Y {
        // tiles: each level from its top row down, each row left to right
        let mut first = std::collections::HashMap::new();
        for c in &chunks {
            let e = first.entry((c.2, c.3)).or_insert(c.4.idx);
            *e = (*e).min(c.4.idx);
        }
        chunks.sort_by_key(|c| (first[&(c.2, c.3)], std::cmp::Reverse(c.1), c.0));
    }
    Ok(chunks)
}

/// The file the port writes for `src` from its pixels: the header copied ([`header_copy`]),
/// then every chunk decoded into planar buffers and encoded by the encode pipeline with its
/// default stages.
fn reencode_file(src: &Context) -> Result<Vec<u8>, Error> {
    let (mut dst, mem) = header_copy(src)?;
    for p in 0..src.get_count() {
        let scan = src.get_storage(p)? == Storage::SCANLINE;
        for (a, b, lx, ly, rinfo) in chunks_in_write_order(src, p)? {
            let planar = |chans: &mut [openexr_core::coding::CodingChannelInfo]| {
                let mut bufs = Vec::new();
                for (i, c) in chans.iter_mut().enumerate() {
                    let bpe = i32::from(c.bytes_per_element);
                    c.user_pixel_stride = bpe;
                    c.user_line_stride = c.width * bpe;
                    c.data = Some(BufferRef {
                        buffer: i,
                        offset: 0,
                    });
                    bufs.push(vec![0u8; (c.width.max(0) * c.height.max(0) * bpe) as usize]);
                }
                bufs
            };
            let mut dec = src.decoding_initialize(p, &rinfo)?;
            let mut bufs = planar(&mut dec.channels);
            src.decoding_choose_default_routines(p, &mut dec)?;
            let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(|b| &mut b[..]).collect();
            src.decoding_run(p, &mut dec, &mut outs)?;

            let winfo = if scan {
                dst.write_scanline_chunk_info(p, a)?
            } else {
                dst.write_tile_chunk_info(p, a, b, lx, ly)?
            };
            let mut enc = dst.encoding_initialize(p, &winfo)?;
            planar(&mut enc.channels);
            dst.encoding_choose_default_routines(p, &mut enc)?;
            let ins: Vec<&[u8]> = bufs.iter().map(|b| &b[..]).collect();
            dst.encoding_run(p, &mut enc, &ins)?;
        }
    }
    let bytes = mem.lock().unwrap().clone();
    Ok(bytes)
}

/// The wheel's file as the port writes it: for a single-part file, without the `type`
/// attribute ([`without_type_attr`]) and with the chunk offsets moved back by its size.
fn as_port_writes(src: &Context, file: &[u8]) -> Vec<u8> {
    if src.get_count() != 1 {
        return file.to_vec();
    }
    let table = src.get_chunk_table_offset(0).unwrap() as usize;
    let n = src.get_chunk_count(0).unwrap() as usize;
    let mut out = without_type_attr(&file[..table]);
    let d = (table - out.len()) as u64;
    for e in file[table..table + 8 * n].as_chunks::<8>().0 {
        out.extend_from_slice(&(u64::from_le_bytes(*e) - d).to_le_bytes());
    }
    out.extend_from_slice(&file[table + 8 * n..]);
    out
}

/// Heavy tier: the files the wheel (the C++ library) writes come back byte for byte when the
/// port encodes their pixels again, but for a single-part file's `type` attribute
/// ([`as_port_writes`]): every scanline and tiled image of [`images`] whose parts all use one
/// of [`ENCODES`]' compressions, in every layout `oracle/write_images.py` writes (subsampled,
/// mixed types, decreasing-Y scanlines and tiles among them). Upstream's test images are
/// compared for NONE and RLE only: older libraries wrote them.
#[test]
fn images_encode_as_the_wheel_wrote_them() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_encode_{}", std::process::id()));
    let mut compared = 0;
    for image in images(&scratch) {
        let name = image.file_name().unwrap().to_string_lossy().into_owned();
        let Ok(src) = Context::start_read(Some(image.to_str().unwrap()), None) else {
            continue;
        };
        // upstream's test images were written by older libraries (ZIP's deflate streams are
        // zlib's, not libdeflate's, and comp_b44_piz.exr's PIZ chunks differ too), so only
        // the wheel's own files check the codecs but NONE and RLE
        let by_wheel = image.parent().is_some_and(|d| d.ends_with("written"));
        let encodable = (0..src.get_count()).all(|p| {
            src.get_compression(p).is_ok_and(|c| {
                ENCODES.contains(&c)
                    && (by_wheel || c == Compression::NONE || c == Compression::RLE)
            }) && src
                .get_storage(p)
                .is_ok_and(|s| s == Storage::SCANLINE || s == Storage::TILED)
        });
        if !encodable || name.contains("_with_") {
            continue;
        }
        let file = as_port_writes(&src, &std::fs::read(&image).unwrap());
        let out = reencode_file(&src).unwrap_or_else(|e| panic!("{name}: {e}"));
        if out != file {
            let at = out
                .iter()
                .zip(&file)
                .position(|(a, b)| a != b)
                .unwrap_or(out.len().min(file.len()));
            panic!(
                "{name}: the file differs at byte {at} (port {} bytes, wheel {})",
                out.len(),
                file.len()
            );
        }
        compared += 1;
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!("files compared: {compared}");
    assert!(compared > 0);
}
