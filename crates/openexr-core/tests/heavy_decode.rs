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
//! interleaved, generic). An image the wheel reads but the port refuses fails the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use openexr_core::coding::BufferRef;
use openexr_core::context::Context;
use openexr_core::openexr_attr::{Compression, Storage};

/// The compressions the port decodes, with the wheel's names for them.
const DECODES: [(Compression, &str); 2] = [
    (Compression::NONE, "NO_COMPRESSION"),
    (Compression::RLE, "RLE_COMPRESSION"),
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

fn decode_part(f: &Context, p: i32) -> Option<(Planes, Vec<Planes>)> {
    let storage = f.get_storage(p).unwrap();
    let comp = f.get_compression(p).unwrap();
    if !DECODES.iter().any(|(c, _)| *c == comp)
        || (storage != Storage::SCANLINE && storage != Storage::TILED)
    {
        return None;
    }
    let dw = f.get_data_window(p).unwrap();

    let probe = if storage == Storage::SCANLINE {
        f.read_scanline_chunk_info(p, dw.min.y).unwrap()
    } else {
        f.read_tile_chunk_info(p, 0, 0, 0, 0).unwrap()
    };
    let first = f.decoding_initialize(p, &probe).unwrap();
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
        let lpc = f.get_scanlines_per_chunk(p).unwrap();
        let mut y = dw.min.y;
        while y <= dw.max.y {
            let cinfo = f.read_scanline_chunk_info(p, y).unwrap();
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
        let (tx_n, ty_n) = f.get_tile_counts(p, 0, 0).unwrap();
        let (tsx, tsy) = f.get_tile_sizes(p, 0, 0).unwrap();
        for ty in 0..ty_n {
            for tx in 0..tx_n {
                let cinfo = f.read_tile_chunk_info(p, tx, ty, 0, 0).unwrap();
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
        let mut d = f.decoding_initialize(p, &ch.cinfo).unwrap();
        for (c, out) in d.channels.iter_mut().enumerate() {
            let k = &chans[c];
            out.data = Some(BufferRef {
                buffer: c,
                offset: (ch.y0[c] * k.w + ch.x0[c]) * k.bpe,
            });
            out.user_pixel_stride = k.bpe as i32;
            out.user_line_stride = (k.w * k.bpe) as i32;
        }
        f.decoding_choose_default_routines(p, &mut d).unwrap();
        let mut outs: Vec<&mut [u8]> = planar.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(p, &mut d, &mut outs).unwrap();
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
        let mut d = f.decoding_initialize(p, &ch.cinfo).unwrap();
        for (c, out) in d.channels.iter_mut().enumerate() {
            let k = &chans[c];
            out.data = Some(BufferRef {
                buffer: c,
                offset: (ch.y0[c] * k.w + ch.x0[c]) * k.bpe * 2,
            });
            out.user_pixel_stride = (k.bpe * 2) as i32;
            out.user_line_stride = (k.w * k.bpe * 2) as i32;
        }
        f.decoding_choose_default_routines(p, &mut d).unwrap();
        let mut outs: Vec<&mut [u8]> = spaced.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(p, &mut d, &mut outs).unwrap();
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
            let mut d = f.decoding_initialize(p, &ch.cinfo).unwrap();
            for (c, out) in d.channels.iter_mut().enumerate() {
                out.data = Some(BufferRef {
                    buffer: 0,
                    offset: ((ch.y0[c] * w + ch.x0[c]) * n + slot(c)) * bpe,
                });
                out.user_pixel_stride = (n * bpe) as i32;
                out.user_line_stride = (w * n * bpe) as i32;
            }
            f.decoding_choose_default_routines(p, &mut d).unwrap();
            let mut outs: Vec<&mut [u8]> = buf.iter_mut().map(Vec::as_mut_slice).collect();
            f.decoding_run(p, &mut d, &mut outs).unwrap();
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
    Some((planar_out, interleaved_out))
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
    let (mut parts_checked, mut layouts_checked, mut parts_skipped) = (0, 0, 0);
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
            let Some((planar, interleaved)) = decode_part(&f, p) else {
                parts_skipped += 1;
                continue;
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
        "parts compared: {parts_checked} ({layouts_checked} layouts); parts for later phases: {parts_skipped}"
    );
    assert!(parts_checked > 0);
}
