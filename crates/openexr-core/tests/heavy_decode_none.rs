// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8): uncompressed images decode bit for bit as the official wheel
//! decodes them, the exit check of Phase 4 (PLAN.md §10).
//!
//! Every image of upstream's test directory is read by the oracle (`oracle/dump_pixels.py`)
//! and by the port. Each part without compression, of scanline or tiled storage, whose
//! channels are not subsampled, is decoded whole (level 0 of a tiled part, the level the wheel
//! reads) into one planar buffer per channel, and compared byte for byte with the wheel's
//! pixels. The other parts wait for their phases: codecs (5), sampling, deep (7). An image
//! the wheel reads but the port refuses fails the check.

use std::path::{Path, PathBuf};
use std::process::Command;

use openexr_core::coding::BufferRef;
use openexr_core::context::Context;
use openexr_core::openexr_attr::{Compression, Storage};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
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
    let root = root();
    let status = Command::new("uv")
        .args(["run", "--project"])
        .arg(root.join("oracle"))
        .args(["python", "-I"])
        .arg(root.join("oracle/dump_pixels.py"))
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

/// The port's decode of part `p`: each channel's name and planar native pixels, or `None`
/// for a part this phase does not decode.
fn decode_part(f: &Context, p: i32) -> Option<Vec<(Vec<u8>, Vec<u8>)>> {
    let storage = f.get_storage(p).unwrap();
    if f.get_compression(p).unwrap() != Compression::NONE
        || (storage != Storage::SCANLINE && storage != Storage::TILED)
    {
        return None;
    }
    let dw = f.get_data_window(p).unwrap();
    let width = (i64::from(dw.max.x) - i64::from(dw.min.x) + 1) as usize;
    let height = (i64::from(dw.max.y) - i64::from(dw.min.y) + 1) as usize;

    // the chunks: (chunk info, x and y of its first pixel in the data window)
    let mut chunks = Vec::new();
    if storage == Storage::SCANLINE {
        let lpc = f.get_scanlines_per_chunk(p).unwrap();
        let mut y = dw.min.y;
        while y <= dw.max.y {
            let cinfo = f.read_scanline_chunk_info(p, y).unwrap();
            chunks.push((cinfo, 0usize, (cinfo.start_y - dw.min.y) as usize));
            y += lpc;
        }
    } else {
        let (tx_n, ty_n) = f.get_tile_counts(p, 0, 0).unwrap();
        let (tsx, tsy) = f.get_tile_sizes(p, 0, 0).unwrap();
        for ty in 0..ty_n {
            for tx in 0..tx_n {
                let cinfo = f.read_tile_chunk_info(p, tx, ty, 0, 0).unwrap();
                chunks.push((cinfo, (tx * tsx) as usize, (ty * tsy) as usize));
            }
        }
    }

    let first = f.decoding_initialize(p, &chunks[0].0).unwrap();
    if first
        .channels
        .iter()
        .any(|c| c.x_samples != 1 || c.y_samples != 1)
    {
        return None;
    }
    let bpe: Vec<usize> = first
        .channels
        .iter()
        .map(|c| c.bytes_per_element as usize)
        .collect();
    let names: Vec<Vec<u8>> = first
        .channels
        .iter()
        .map(|c| c.channel_name.clone())
        .collect();
    drop(first);
    let mut bufs: Vec<Vec<u8>> = bpe.iter().map(|b| vec![0u8; width * height * b]).collect();

    for (cinfo, x0, y0) in &chunks {
        let mut decoder = f.decoding_initialize(p, cinfo).unwrap();
        for (c, ch) in decoder.channels.iter_mut().enumerate() {
            ch.data = Some(BufferRef {
                buffer: c,
                offset: (y0 * width + x0) * bpe[c],
            });
            ch.user_pixel_stride = bpe[c] as i32;
            ch.user_line_stride = (width * bpe[c]) as i32;
        }
        f.decoding_choose_default_routines(p, &mut decoder).unwrap();
        let mut outs: Vec<&mut [u8]> = bufs.iter_mut().map(Vec::as_mut_slice).collect();
        f.decoding_run(p, &mut decoder, &mut outs).unwrap();
    }
    Some(names.into_iter().zip(bufs).collect())
}

#[test]
fn uncompressed_images_decode_as_the_wheel_does() {
    let dir = root().join("upstream/openexr/src/test/OpenEXRTest");
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_none_{}", std::process::id()));
    let mut images: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("test images")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "exr"))
        .collect();
    images.sort();

    let (mut parts_checked, mut parts_skipped) = (0, 0);
    for (n, image) in images.iter().enumerate() {
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
            let Some(chans) = decode_part(&f, p) else {
                parts_skipped += 1;
                continue;
            };
            for (name, data) in chans {
                let w = wheel
                    .iter()
                    .find(|c| c.part == p && c.name == name)
                    .unwrap_or_else(|| {
                        panic!(
                            "{}: part {p} channel {:?} not in the wheel's",
                            image.display(),
                            name
                        )
                    });
                assert!(
                    w.data == data,
                    "{}: part {p} channel {}: pixels differ",
                    image.display(),
                    String::from_utf8_lossy(&name)
                );
            }
            parts_checked += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    eprintln!(
        "uncompressed parts compared: {parts_checked}; parts for later phases: {parts_skipped}"
    );
    assert!(parts_checked > 0);
}
