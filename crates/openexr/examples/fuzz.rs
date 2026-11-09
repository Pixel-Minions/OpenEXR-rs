// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! A mutation fuzzer of the API's reads: files from a list, damaged at random (bits flipped,
//! bytes overwritten, integers planted, cut, bytes inserted), each read every way the API
//! reads (opening, `Image::read`, every level of every tiled part, line ranges, on one thread
//! and on several). A read may fail, never panic, hang or abort.
//!
//!   cargo run --release -p openexr-rs --example fuzz -- <list.txt> <iterations> <seed> <out-dir>
//!
//! Before each case the damaged file is written to <out-dir>/current.exr, so that a process
//! that aborts (an allocation it cannot make) leaves its cause behind; a case that panics is
//! kept as <out-dir>/panic-<n>.exr, one slower than 10 s as slow-<n>.exr. The exit status is
//! the number of panics (capped at 100). Deterministic for a seed. Also
//!
//!   fuzz write <seed>       one random write again (FUZZ_VERBOSE=1 shows its header)
//!
//! `tests/heavy_fuzz.rs` runs it on every `gate --full`.

use std::time::{Duration, Instant};

use openexr::{FrameBuffer, Image, MultiPartInputFile, ReadOptions, Slice, Storage, Threads};

/// splitmix64: a fixed sequence for a seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// Integers a reader's checks hinge on.
const INTERESTING: [u32; 14] = [
    0,
    1,
    2,
    0x7F,
    0x80,
    0xFF,
    0x7FFF,
    0x8000,
    0xFFFF,
    0x7FFF_FFFF,
    0x8000_0000,
    0xFFFF_FFFF,
    0x0001_0000,
    0x00FF_FFFF,
];

/// `bytes` damaged once or a few times. The header (the first few hundred bytes) and the
/// chunk table after it are hit more often than the chunks, where most readers' decisions
/// are made.
fn mutate(rng: &mut Rng, bytes: &[u8]) -> Vec<u8> {
    let mut b = bytes.to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        if b.is_empty() {
            break;
        }
        let near = (b.len()).min(2048);
        let at = if rng.below(3) > 0 {
            rng.below(near)
        } else {
            rng.below(b.len())
        };
        match rng.below(6) {
            0 => {
                // bits flipped
                for _ in 0..1 + rng.below(8) {
                    let i = if rng.below(2) == 0 {
                        at
                    } else {
                        rng.below(b.len())
                    };
                    b[i] ^= 1 << rng.below(8);
                }
            }
            1 => {
                // a range overwritten
                let n = (1 + rng.below(16)).min(b.len() - at);
                let v = [0x00, 0xFF, rng.next() as u8][rng.below(3)];
                b[at..at + n].fill(v);
            }
            2 => {
                // an integer planted, little-endian as the file's
                let v = INTERESTING[rng.below(INTERESTING.len())];
                let v = if rng.below(4) == 0 {
                    v.wrapping_add(rng.below(3) as u32).wrapping_sub(1)
                } else {
                    v
                };
                let bytes4 = v.to_le_bytes();
                let n = 4.min(b.len() - at);
                b[at..at + n].copy_from_slice(&bytes4[..n]);
            }
            3 => {
                // a 64-bit integer planted (chunk offsets, sizes)
                let v: u64 =
                    [0, u64::MAX, 1 << 32, 1 << 62, rng.next() >> rng.below(64)][rng.below(5)];
                let bytes8 = v.to_le_bytes();
                let n = 8.min(b.len() - at);
                b[at..at + n].copy_from_slice(&bytes8[..n]);
            }
            4 => {
                // cut
                b.truncate(at.max(1));
            }
            _ => {
                // bytes inserted
                let n = 1 + rng.below(8);
                let fill: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                b.splice(at..at, fill);
            }
        }
    }
    b
}

/// Every read the API makes of `bytes`. Errors are the expected outcome of most cases.
fn exercise(bytes: Vec<u8>, threads: Threads, layout_seed: Option<u64>) {
    // limits, as a reader of untrusted files sets them: no header sizes a vast buffer
    let opts = ReadOptions::new()
        .threads(threads)
        .max_image_size(16384, 16384)
        .max_tile_size(4096, 4096);
    let Ok(file) = opts.open_bytes("<fuzz>", bytes) else {
        return;
    };
    if let Some(seed) = layout_seed {
        random_layouts(&file, &mut Rng(seed));
        return;
    }
    let _ = Image::from_file(&file);
    read_levels_and_lines(&file);
}

/// Every level of every tiled part, tile range by tile range, and a few line ranges of every
/// scanline part, into planes of the level (each capped, so that a header asking for a vast
/// window is refused by the API's checks, not by this program's memory).
fn read_levels_and_lines(file: &MultiPartInputFile) {
    const CAP: u64 = 64 << 20;
    for p in 0..file.parts() {
        let Ok(part) = file.part(p) else { continue };
        let h = part.header();
        match h.storage() {
            Storage::Tiled => {
                let Ok((nlx, nly)) = part.num_levels() else {
                    continue;
                };
                for ly in 0..nly.min(16) {
                    for lx in 0..nlx.min(16) {
                        if !part.is_valid_level(lx, ly) {
                            continue;
                        }
                        let Ok(w) = part.data_window_for_level((lx, ly)) else {
                            continue;
                        };
                        let Ok((nx, ny)) = part.num_tiles((lx, ly)) else {
                            continue;
                        };
                        let px = w.width() * w.height();
                        if px == 0 || px * 4 > CAP {
                            continue;
                        }
                        let mut planes: Vec<Vec<u8>> = h
                            .channels()
                            .iter()
                            .map(|c| vec![0u8; px as usize * c.pixel_type.size()])
                            .collect();
                        let mut fb = FrameBuffer::new();
                        for (c, plane) in h.channels().iter().zip(planes.iter_mut()) {
                            let size = c.pixel_type.size() as i64;
                            let id = fb.add_bytes(plane);
                            let _ = fb.insert(
                                &c.name,
                                Slice::new(
                                    c.pixel_type,
                                    id,
                                    0,
                                    w.min,
                                    size,
                                    size * w.width() as i64,
                                ),
                            );
                        }
                        let _ = part.read_tiles(&mut fb, (0, nx - 1), (0, ny - 1), (lx, ly));
                        let _ = part.read_tile(&mut fb, nx - 1, ny - 1, (lx, ly));
                    }
                }
            }
            Storage::Scanline => {
                let dw = h.data_window();
                let px = dw.width() * dw.height();
                if px == 0 || px * 4 > CAP {
                    continue;
                }
                let mut planes: Vec<Vec<u8>> = h
                    .channels()
                    .iter()
                    .map(|c| vec![0u8; px as usize * c.pixel_type.size()])
                    .collect();
                let mut fb = FrameBuffer::new();
                for (c, plane) in h.channels().iter().zip(planes.iter_mut()) {
                    let size = c.pixel_type.size() as i64;
                    let id = fb.add_bytes(plane);
                    let mut s =
                        Slice::new(c.pixel_type, id, 0, dw.min, size, size * dw.width() as i64);
                    s.x_sampling = c.x_sampling.max(1);
                    s.y_sampling = c.y_sampling.max(1);
                    let _ = fb.insert(&c.name, s);
                }
                let mid = dw.min.y + (dw.max.y - dw.min.y) / 2;
                let _ = part.read_pixels(&mut fb, mid, dw.max.y);
                let _ = part.read_pixels(&mut fb, dw.min.y, dw.min.y);
            }
            _ => {}
        }
    }
}

/// Reads of an undamaged file into frame buffers of random layouts: strides of either sign or
/// none, offsets anywhere, buffers of any size, sampling right or wrong, channels the file has
/// and has not, other pixel types, line ranges and tiles. The API checks every slice against
/// its buffer before Core writes a byte: a read may be refused, never panic.
fn random_layouts(file: &MultiPartInputFile, rng: &mut Rng) {
    for p in 0..file.parts() {
        let Ok(part) = file.part(p) else { continue };
        let h = part.header();
        let dw = h.data_window();
        if dw.width() * dw.height() > (1 << 22) {
            continue;
        }
        for _ in 0..4 {
            let mut bufs: Vec<Vec<u8>> = (0..1 + rng.below(3))
                .map(|_| vec![0u8; rng.below(dw.width() as usize * dw.height() as usize * 8 + 64)])
                .collect();
            let nbuf = bufs.len();
            let mut fb = FrameBuffer::new();
            let ids: Vec<usize> = bufs.iter_mut().map(|b| fb.add_bytes(b)).collect();
            let mut names: Vec<Vec<u8>> = h.channels().iter().map(|c| c.name.clone()).collect();
            names.push(b"not in the file".to_vec());
            for name in &names {
                if rng.below(4) == 0 {
                    continue;
                }
                let t = [
                    openexr::PixelType::Half,
                    openexr::PixelType::Float,
                    openexr::PixelType::Uint,
                ][rng.below(3)];
                let size = t.size() as i64;
                let stride = |rng: &mut Rng| -> i64 {
                    match rng.below(5) {
                        0 => 0,
                        1 => -(size * (1 + rng.below(8) as i64)),
                        _ => size * (1 + rng.below(8) as i64),
                    }
                };
                let xs = stride(rng);
                let ys = stride(rng) * (1 + rng.below(dw.width() as usize + 1) as i64);
                let buf = ids[rng.below(nbuf)];
                let mut s = Slice::new(
                    t,
                    buf,
                    rng.below(bufs_len(&fb, buf) + 8),
                    openexr::V2 {
                        x: dw.min.x + rng.below(5) as i32 - 2,
                        y: dw.min.y + rng.below(5) as i32 - 2,
                    },
                    xs,
                    ys,
                );
                s.x_sampling = 1 + rng.below(3) as i32;
                s.y_sampling = 1 + rng.below(3) as i32;
                if rng.below(2) == 0
                    && let Some(c) = h.channel(name)
                {
                    s.x_sampling = c.x_sampling;
                    s.y_sampling = c.y_sampling;
                }
                s.fill_value = [0.0, 1.0, -1.0, f64::NAN, 1e30, 70000.0][rng.below(6)];
                let _ = fb.insert(name, s);
            }
            let h2 = dw.height().max(1) as usize;
            let y1 = dw.min.y + rng.below(h2) as i32 - 1;
            let y2 = dw.min.y + rng.below(h2) as i32 + 1;
            if h.storage() == Storage::Tiled && rng.below(2) == 0 {
                if let Ok((nx, ny)) = part.num_tiles((0, 0)) {
                    let tx = rng.below(nx.max(1) as usize + 1) as i32;
                    let ty = rng.below(ny.max(1) as usize + 1) as i32;
                    let _ = part.read_tiles(&mut fb, (0, tx), (ty, 0), (0, 0));
                }
            } else {
                let _ = part.read_pixels(&mut fb, y1, y2);
            }
        }
    }
}

/// Random writes that ended a file.
static WRITTEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A write of a random file: a header of random channels (types, sampling), window,
/// compression, line order, tiles and levels, scanline or deep; its pixels from buffers of
/// random layouts, given in random amounts and orders, then the file ended. A write may be
/// refused, never panic; a file it ends must read back.
fn random_write(rng: &mut Rng) {
    use openexr::{
        Box2i, Channel, Compression, DeepFrameBuffer, Header, LevelMode, LineOrder, PixelType,
        RoundingMode, TileDescription, V2, WriteOptions,
    };
    let w = 1 + rng.below(40) as i32;
    let ht = 1 + rng.below(40) as i32;
    let x0 = rng.below(9) as i32 - 4;
    let y0 = rng.below(9) as i32 - 4;
    let dw = Box2i {
        min: V2 { x: x0, y: y0 },
        max: V2 {
            x: x0 + w - 1,
            y: y0 + ht - 1,
        },
    };
    let mut h = Header::new(dw);
    let deep = rng.below(5) == 0;
    if deep {
        h.set_storage(Storage::DeepScanline);
    }
    let types = [PixelType::Half, PixelType::Float, PixelType::Uint];
    for k in 0..1 + rng.below(4) {
        let mut c = Channel::new(format!("c{k}"), types[rng.below(3)]);
        if !deep && rng.below(4) == 0 {
            c.x_sampling = 1 + rng.below(3) as i32;
            c.y_sampling = 1 + rng.below(3) as i32;
        }
        h.insert_channel(c);
    }
    h.set_compression(Compression::from_value(rng.below(14) as u8));
    h.set_line_order(LineOrder::from_value(rng.below(3) as u8));
    if rng.below(2) == 0 {
        h.set_tiles(TileDescription {
            x_size: 1 + rng.below(20) as u32,
            y_size: 1 + rng.below(20) as u32,
            level_mode: LevelMode::from_value(rng.below(3) as u8),
            rounding_mode: RoundingMode::from_value(rng.below(2) as u8),
        });
    }
    if std::env::var_os("FUZZ_VERBOSE").is_some() {
        eprintln!(
            "{:?} {:?} {:?} {:?} {:?}",
            h.storage(),
            dw,
            h.compression(),
            h.line_order(),
            h.tiles()
        );
        for c in h.channels() {
            eprintln!(
                "  {:?} {:?} {} {}",
                String::from_utf8_lossy(&c.name),
                c.pixel_type,
                c.x_sampling,
                c.y_sampling
            );
        }
    }
    let opts = WriteOptions::new().threads(if rng.below(2) == 0 {
        Threads::Serial
    } else {
        Threads::Count(3)
    });
    let Ok(mut out) = opts.create_writer(
        "<fuzz>",
        std::io::Cursor::new(Vec::new()),
        std::slice::from_ref(&h),
    ) else {
        return;
    };
    let n = (w * ht) as usize;
    let data: Vec<Vec<u8>> = (0..3)
        .map(|_| {
            (0..n * 8 + rng.below(64))
                .map(|_| rng.next() as u8)
                .collect()
        })
        .collect();
    if rng.below(2) == 0 {
        complete_write(rng, out, &h, dw);
        return;
    }
    let Ok(mut part) = out.part(0) else { return };
    for _ in 0..1 + rng.below(6) {
        if deep {
            let counts: Vec<u32> = (0..n).map(|_| rng.below(3) as u32).collect();
            let Ok(mut fb) = DeepFrameBuffer::new(&counts, dw) else {
                continue;
            };
            for c in h.channels() {
                if rng.below(4) > 0 {
                    let _ = fb.insert_bytes(&c.name, types[rng.below(3)], &data[rng.below(3)][..]);
                }
            }
            if h.storage() == Storage::DeepTiled {
                let _ = part.write_deep_tile(&fb, rng.below(4) as i32, rng.below(4) as i32, (0, 0));
            } else {
                let _ = part.write_deep_pixels(&fb, rng.below(ht as usize + 2) as i32);
            }
            continue;
        }
        let mut fb = FrameBuffer::new();
        let ids: Vec<usize> = data.iter().map(|d| fb.add_bytes(&d[..])).collect();
        for c in h.channels() {
            if rng.below(4) == 0 {
                continue;
            }
            let t = if rng.below(4) == 0 {
                types[rng.below(3)]
            } else {
                c.pixel_type
            };
            let size = t.size() as i64;
            let xs = [size, 0, -size, size * 2][rng.below(4)];
            let ys = xs * (w as i64 + rng.below(3) as i64);
            let mut s = Slice::new(t, ids[rng.below(3)], rng.below(n * 4), dw.min, xs, ys);
            s.x_sampling = c.x_sampling;
            s.y_sampling = c.y_sampling;
            let _ = fb.insert(&c.name, s);
        }
        if h.storage().is_tiled() {
            let lvl = rng.below(3) as i32;
            let level = (lvl, if rng.below(2) == 0 { lvl } else { 0 });
            if let Ok((nx, ny)) = part.num_tiles(level) {
                let _ = part.write_tiles(
                    &fb,
                    (
                        rng.below(nx as usize + 1) as i32,
                        rng.below(nx as usize + 1) as i32,
                    ),
                    (
                        rng.below(ny as usize + 1) as i32,
                        rng.below(ny as usize + 1) as i32,
                    ),
                    level,
                );
            }
        } else {
            let _ = part.write_pixels(&fb, rng.below(ht as usize + 2) as i32);
        }
    }
    if let Ok(cursor) = out.finish() {
        WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // a file the writer ends reads back
        let bytes = cursor.into_inner();
        let file = ReadOptions::new()
            .open_bytes("<fuzz>", bytes)
            .expect("a file the API wrote opens");
        let _ = Image::from_file(&file);
    }
}

/// Every line, tile and level of `h`'s part written from valid buffers, in random amounts and
/// orders: the file must end, read back, and with a lossless compression give back exactly
/// what was written (level 0, as `Image::read` reads it).
fn complete_write(
    rng: &mut Rng,
    mut out: openexr::MultiPartOutputFile<std::io::Cursor<Vec<u8>>>,
    h: &openexr::Header,
    dw: openexr::Box2i,
) {
    use openexr::Compression as C;
    let (level0, counts0) = match write_everything(rng, &mut out, h, dw) {
        Ok(v) => v,
        Err(e) => {
            // a refusal part-way (a codec that cannot take the header: OpenJPH's
            // power-of-two sampling): the file is failed, and ends with that error
            let f = out.finish();
            assert!(
                f.as_ref().err() == Some(&e),
                "a failed file ends with its error: {e} / {f:?}"
            );
            return;
        }
    };
    let deep = h.storage().is_deep();
    let bytes = out
        .finish()
        .expect("a file written whole ends")
        .into_inner();
    WRITTEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let file = ReadOptions::new()
        .open_bytes("<fuzz>", bytes)
        .expect("a file the API wrote opens");
    let img = Image::from_file(&file).expect("a file the API wrote reads");
    let lossless = [C::None, C::Rle, C::Zips, C::Zip, C::Piz, C::Zstd].contains(&h.compression());
    if !lossless {
        return;
    }
    let p = &img.parts[0];
    if deep {
        assert_eq!(
            p.sample_counts.as_ref(),
            Some(&counts0),
            "deep counts read back"
        );
    }
    for (ch, want) in p.channels.iter().zip(&level0) {
        assert!(
            ch.pixels.as_bytes() == &want[..],
            "{:?}: channel {} reads back as written",
            h.compression(),
            String::from_utf8_lossy(&ch.name)
        );
    }
}

/// Every line, tile and level of `out`'s part written; level 0's planes (or a deep part's
/// samples) and counts.
fn write_everything(
    rng: &mut Rng,
    out: &mut openexr::MultiPartOutputFile<std::io::Cursor<Vec<u8>>>,
    h: &openexr::Header,
    dw: openexr::Box2i,
) -> openexr::Result<(Vec<Vec<u8>>, Vec<u32>)> {
    use openexr::DeepFrameBuffer;
    let sampled = |lo: i32, hi: i32, s: i32| (lo..=hi).filter(|v| v % s == 0).count();
    let random =
        |rng: &mut Rng, n: usize| -> Vec<u8> { (0..n).map(|_| rng.next() as u8).collect() };
    let deep = h.storage().is_deep();
    let tiled = h.storage().is_tiled();
    let mut part = out.part(0)?;
    // level 0's planes (or counts and samples), as written
    let mut level0: Vec<Vec<u8>> = Vec::new();
    let mut counts0: Vec<u32> = Vec::new();
    let levels: Vec<(i32, i32)> = if tiled {
        (0..16)
            .flat_map(|ly| (0..16).map(move |lx| (lx, ly)))
            .filter(|&l| part.num_tiles(l).is_ok())
            .collect()
    } else {
        vec![(0, 0)]
    };
    for &level in &levels {
        let window = if tiled {
            let (lw, lh) = part.level_size(level)?;
            openexr::Box2i {
                min: dw.min,
                max: openexr::V2 {
                    x: dw.min.x + lw - 1,
                    y: dw.min.y + lh - 1,
                },
            }
        } else {
            dw
        };
        let n = (window.width() * window.height()) as usize;
        if deep {
            let counts: Vec<u32> = (0..n).map(|_| rng.below(3) as u32).collect();
            let total: usize = counts.iter().map(|&c| c as usize).sum();
            let samples: Vec<Vec<u8>> = h
                .channels()
                .iter()
                .map(|c| random(rng, total * c.pixel_type.size()))
                .collect();
            let mut fb = DeepFrameBuffer::new(&counts, window).expect("valid counts");
            for (c, s) in h.channels().iter().zip(&samples) {
                fb.insert_bytes(&c.name, c.pixel_type, &s[..])
                    .expect("valid samples");
            }
            if tiled {
                let (nx, ny) = part.num_tiles(level).expect("valid level");
                let mut tiles: Vec<(i32, i32)> =
                    (0..ny).flat_map(|y| (0..nx).map(move |x| (x, y))).collect();
                for i in (1..tiles.len()).rev() {
                    tiles.swap(i, rng.below(i + 1));
                }
                for (x, y) in tiles {
                    part.write_deep_tile(&fb, x, y, level)?;
                }
            } else {
                let mut left = window.height() as i32;
                while left > 0 {
                    let k = 1 + rng.below(left as usize) as i32;
                    part.write_deep_pixels(&fb, k)?;
                    left -= k;
                }
            }
            drop(fb);
            if level == (0, 0) {
                counts0 = counts;
                level0 = samples;
            }
            continue;
        }
        let planes: Vec<Vec<u8>> = h
            .channels()
            .iter()
            .map(|c| {
                let k = sampled(window.min.x, window.max.x, c.x_sampling)
                    * sampled(window.min.y, window.max.y, c.y_sampling);
                random(rng, k * c.pixel_type.size())
            })
            .collect();
        let mut fb = FrameBuffer::new();
        for (c, plane) in h.channels().iter().zip(&planes) {
            let size = c.pixel_type.size() as i64;
            let w = sampled(window.min.x, window.max.x, c.x_sampling) as i64;
            let id = fb.add_bytes(&plane[..]);
            let mut s = Slice::new(c.pixel_type, id, 0, window.min, size, size * w);
            s.x_sampling = c.x_sampling;
            s.y_sampling = c.y_sampling;
            fb.insert(&c.name, s).expect("a valid slice");
        }
        if tiled {
            let (nx, ny) = part.num_tiles(level).expect("valid level");
            let mut tiles: Vec<(i32, i32)> =
                (0..ny).flat_map(|y| (0..nx).map(move |x| (x, y))).collect();
            for i in (1..tiles.len()).rev() {
                tiles.swap(i, rng.below(i + 1));
            }
            for (x, y) in tiles {
                part.write_tile(&fb, x, y, level)?;
            }
        } else {
            let mut left = window.height() as i32;
            while left > 0 {
                let k = 1 + rng.below(left as usize) as i32;
                part.write_pixels(&fb, k)?;
                left -= k;
            }
        }
        drop(fb);
        if level == (0, 0) {
            level0 = planes;
        }
    }
    Ok((level0, counts0))
}

fn bufs_len(fb: &FrameBuffer<'_>, _buf: usize) -> usize {
    // the slices' offsets are drawn past the buffers' ends too: any value goes
    let _ = fb;
    1 << 16
}

/// What a run of the fuzzer found: its cases, the panics (each described), the slow cases, and
/// the random writes that ended a file.
pub(crate) struct Report {
    pub(crate) cases: usize,
    pub(crate) panics: Vec<String>,
    pub(crate) slow: usize,
    pub(crate) written: usize,
}

/// `iterations` cases from `seeds` (each a file's bytes), deterministic for `seed`; each case
/// written to `out`/current.exr before it runs, a panicking one kept as panic-<n>.exr, a slow
/// one (over `slow`) as slow-<n>.exr.
pub(crate) fn run(
    seeds: &[Vec<u8>],
    iterations: usize,
    seed: u64,
    out: &std::path::Path,
    slow: Duration,
) -> std::io::Result<Report> {
    std::fs::create_dir_all(out)?;
    let mut rng = Rng(seed);
    let mut report = Report {
        cases: iterations,
        panics: Vec::new(),
        slow: 0,
        written: 0,
    };
    let written_before = WRITTEN.load(std::sync::atomic::Ordering::Relaxed);
    let message = |e: Box<dyn std::any::Any + Send>| {
        e.downcast_ref::<String>()
            .cloned()
            .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default()
    };
    for n in 0..iterations {
        let base = &seeds[rng.below(seeds.len())];
        // one case in eight: a random write; one in eight: the file undamaged, read into
        // random layouts; the rest: the file damaged
        if n % 8 == 2 {
            let ws = rng.next();
            if let Err(e) = std::panic::catch_unwind(move || random_write(&mut Rng(ws))) {
                report
                    .panics
                    .push(format!("write case {n} (fuzz write {ws}): {}", message(e)));
            }
            continue;
        }
        let layout = (n % 8 == 5).then(|| rng.next());
        let case = if layout.is_some() {
            base.clone()
        } else {
            mutate(&mut rng, base)
        };
        std::fs::write(out.join("current.exr"), &case)?;
        let threads = if n % 4 == 3 {
            Threads::Count(3)
        } else {
            Threads::Serial
        };
        let t = Instant::now();
        let c = case.clone();
        if let Err(e) = std::panic::catch_unwind(move || exercise(c, threads, layout)) {
            report.panics.push(format!("case {n}: {}", message(e)));
            std::fs::write(out.join(format!("panic-{n}.exr")), &case)?;
        }
        if t.elapsed() > slow {
            report.slow += 1;
            std::fs::write(out.join(format!("slow-{n}.exr")), &case)?;
        }
    }
    let _ = std::fs::remove_file(out.join("current.exr"));
    report.written = WRITTEN.load(std::sync::atomic::Ordering::Relaxed) - written_before;
    Ok(report)
}

#[allow(dead_code)] // the heavy test runs `run`
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    // fuzz write <seed>: one random write again, its header shown (FUZZ_VERBOSE)
    if args.len() == 3 && args[1] == "write" {
        random_write(&mut Rng(args[2].parse()?));
        println!("ok");
        return Ok(());
    }
    if args.len() != 5 {
        return Err("usage: fuzz <list.txt> <iterations> <seed> <out-dir>".into());
    }
    let seeds: Vec<Vec<u8>> = std::fs::read_to_string(&args[1])?
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|p| std::fs::read(p).ok())
        .filter(|b| b.len() < (16 << 20))
        .collect();
    if seeds.is_empty() {
        return Err("no seed files".into());
    }
    std::panic::set_hook(Box::new(|_| {}));
    let started = Instant::now();
    let report = run(
        &seeds,
        args[2].parse()?,
        args[3].parse()?,
        std::path::Path::new(&args[4]),
        Duration::from_secs(10),
    )?;
    for p in &report.panics {
        println!("panic at {p}");
    }
    println!(
        "{} cases in {:.1} s: {} panics, {} slow; {} random writes ended a file",
        report.cases,
        started.elapsed().as_secs_f64(),
        report.panics.len(),
        report.slow,
        report.written
    );
    std::process::exit(report.panics.len().min(100) as i32);
}
