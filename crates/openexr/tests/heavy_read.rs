// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, §16 M4.1): every flat part of the decode set, read through the
//! API's `FrameBuffer` and `InputPart::read_pixels`, gives the wheel's pixels bit for bit.
//!
//! The wheel reads each part as `InputPart::readPixels (dw.min.y, dw.max.y)` into one array
//! per channel (PyOpenEXR.cpp:476-604 @ v3.5.3; `oracle/dump_pixels.py`). Each part is read
//! here whole into typed planes, whole into one interleaved buffer when its channels share a
//! type and are not sampled, and in four line ranges into planes the size of the data window,
//! each compared with the wheel's arrays (the rows of the range; the rest must be left as
//! they were). A part the wheel cannot read must fail through the API too, and the reverse.
//! Deep parts are M4.2's.

mod common;

use common::{WheelChannel, images, oracle, root, wheel_pixels, write_list};
use openexr::{FrameBuffer, InputPart, MultiPartInputFile, PixelType, Slice, Storage};

/// The bytes untouched samples keep: a pattern, not a pixel value.
const UNTOUCHED: u8 = 0xA5;

fn pixel_type(dtype: &str) -> PixelType {
    match dtype {
        "float16" => PixelType::Half,
        "float32" => PixelType::Float,
        "uint32" => PixelType::Uint,
        _ => panic!("unexpected dtype {dtype}"),
    }
}

/// A typed plane, to read through `insert_sampled_plane`.
enum Plane {
    Half(Vec<u16>),
    Float(Vec<f32>),
    Uint(Vec<u32>),
}

impl Plane {
    fn new(t: PixelType, n: usize) -> Plane {
        match t {
            PixelType::Half => Plane::Half(vec![0; n]),
            PixelType::Float => Plane::Float(vec![0.0; n]),
            _ => Plane::Uint(vec![0; n]),
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Plane::Half(v) => bytemuck::cast_slice(v),
            Plane::Float(v) => bytemuck::cast_slice(v),
            Plane::Uint(v) => bytemuck::cast_slice(v),
        }
    }
}

/// How many of `lo..=hi` are multiples of `s`.
fn samples(lo: i32, hi: i32, s: i32) -> usize {
    (lo..=hi).filter(|v| v % s == 0).count()
}

/// The part read whole into one typed plane per channel of its header, by name.
fn read_planar(part: &InputPart<'_>) -> openexr::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let h = part.header();
    let dw = h.data_window();
    let mut planes: Vec<Plane> = h
        .channels()
        .iter()
        .map(|c| {
            let n = samples(dw.min.x, dw.max.x, c.x_sampling)
                * samples(dw.min.y, dw.max.y, c.y_sampling);
            Plane::new(c.pixel_type, n)
        })
        .collect();
    let mut fb = FrameBuffer::new();
    for (c, plane) in h.channels().iter().zip(planes.iter_mut()) {
        let (xs, ys) = (c.x_sampling, c.y_sampling);
        match plane {
            Plane::Half(v) => fb.insert_sampled_plane(&c.name, v, dw, xs, ys)?,
            Plane::Float(v) => fb.insert_sampled_plane(&c.name, v, dw, xs, ys)?,
            Plane::Uint(v) => fb.insert_sampled_plane(&c.name, v, dw, xs, ys)?,
        };
    }
    part.read_pixels(&mut fb, dw.min.y, dw.max.y)?;
    drop(fb);
    Ok(h.channels()
        .iter()
        .zip(&planes)
        .map(|(c, p)| (c.name.clone(), p.bytes().to_vec()))
        .collect())
}

/// The part read whole into one buffer, channel after channel in each pixel (in `order`),
/// then split back into planes.
fn read_interleaved(
    part: &InputPart<'_>,
    wheel: &[&WheelChannel],
    order: &[usize],
) -> openexr::Result<Vec<Vec<u8>>> {
    let dw = part.header().data_window();
    let t = pixel_type(&wheel[0].dtype);
    let size = t.size();
    let n = wheel.len();
    let pixels = wheel[0].width * wheel[0].height;
    let mut buf = vec![0u8; pixels * n * size];
    let mut fb = FrameBuffer::new();
    let b = fb.add_bytes(&mut buf);
    for (slot, &c) in order.iter().enumerate() {
        let s = Slice::new(
            t,
            b,
            slot * size,
            dw.min,
            (n * size) as i64,
            (n * size * wheel[0].width) as i64,
        );
        fb.insert(&wheel[c].name, s)?;
    }
    part.read_pixels(&mut fb, dw.min.y, dw.max.y)?;
    drop(fb);
    let mut planes = vec![Vec::with_capacity(pixels * size); n];
    for px in buf.chunks_exact(n * size) {
        for (slot, &c) in order.iter().enumerate() {
            planes[c].extend_from_slice(&px[slot * size..(slot + 1) * size]);
        }
    }
    Ok(planes)
}

/// Lines `y1..=y2` read into planes of the whole data window, filled with [`UNTOUCHED`]
/// first.
fn read_lines(
    part: &InputPart<'_>,
    wheel: &[&WheelChannel],
    y1: i32,
    y2: i32,
) -> openexr::Result<Vec<Vec<u8>>> {
    let dw = part.header().data_window();
    let mut planes: Vec<Vec<u8>> = wheel
        .iter()
        .map(|c| vec![UNTOUCHED; c.data.len()])
        .collect();
    let mut fb = FrameBuffer::new();
    for (c, plane) in wheel.iter().zip(planes.iter_mut()) {
        let t = pixel_type(&c.dtype);
        let size = t.size() as i64;
        let b = fb.add_bytes(plane);
        let mut s = Slice::new(t, b, 0, dw.min, size, size * c.width as i64);
        s.x_sampling = c.x_sampling;
        s.y_sampling = c.y_sampling;
        fb.insert(&c.name, s)?;
    }
    part.read_pixels(&mut fb, y1, y2)?;
    drop(fb);
    Ok(planes)
}

/// The wheel's plane with only the rows of lines `y1..=y2` kept, the rest [`UNTOUCHED`].
fn wheel_rows(c: &WheelChannel, min_y: i32, y1: i32, y2: i32) -> Vec<u8> {
    let row = c.data.len() / c.height.max(1);
    let mut out = vec![UNTOUCHED; c.data.len()];
    // Core requires the data window to start on a sample (validation.c)
    for r in 0..c.height {
        let y = min_y + r as i32 * c.y_sampling;
        if (y1..=y2).contains(&y) {
            out[r * row..(r + 1) * row].copy_from_slice(&c.data[r * row..(r + 1) * row]);
        }
    }
    out
}

#[test]
fn pixels_read_as_the_wheel_reads_them() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_api_read_{}", std::process::id()));
    let images = images(&scratch);
    let list = scratch.join("images.txt");
    write_list(&list, &images);
    let out = scratch.join("pixels");
    std::fs::create_dir_all(&out).unwrap();
    let script = root().join("oracle/dump_pixels.py");
    oracle(&[
        script.as_os_str(),
        "--many".as_ref(),
        list.as_os_str(),
        out.as_os_str(),
    ]);

    let (mut parts, mut reads, mut both_refuse) = (0, 0, 0);
    for (n, image) in images.iter().enumerate() {
        let Ok(wheel) = wheel_pixels(&out.join(n.to_string())) else {
            continue; // the wheel refuses the file: the headers check compares refusals
        };
        let file = MultiPartInputFile::open(image).unwrap_or_else(|e| {
            panic!(
                "{}: the wheel reads it, the API refuses it: {e}",
                image.display()
            )
        });
        let threaded = openexr::ReadOptions::new()
            .threads(openexr::Threads::Count(4))
            .open(image)
            .unwrap();
        for (p, h) in file.headers().iter().enumerate() {
            if !matches!(h.storage(), Storage::Scanline | Storage::Tiled) {
                continue;
            }
            let part = file.part(p).unwrap();
            let chans: Vec<&WheelChannel> = wheel.iter().filter(|c| c.part == p).collect();
            let what = |how: &str| format!("{}: part {p}, {how}", image.display());
            let planar = read_planar(&part);
            if chans.is_empty() {
                // the wheel left the part out: it could not read its pixels
                if h.channels().is_empty() {
                    continue;
                }
                assert!(
                    planar.is_err(),
                    "{}",
                    what("the wheel cannot read it, the API reads it")
                );
                both_refuse += 1;
                continue;
            }
            let planar = planar.unwrap_or_else(|e| {
                panic!("{}: {e}", what("the wheel reads it, the API refuses it"))
            });
            assert_eq!(chans.len(), planar.len(), "{}", what("channels"));
            for c in &chans {
                let got = planar.iter().find(|(n, _)| *n == c.name).map(|(_, d)| d);
                assert!(
                    got == Some(&c.data),
                    "{}",
                    what(&format!("planar, {}", String::from_utf8_lossy(&c.name)))
                );
            }
            reads += 1;

            let uniform = chans.len() > 1
                && chans
                    .iter()
                    .all(|c| c.dtype == chans[0].dtype && c.x_sampling == 1 && c.y_sampling == 1);
            if uniform {
                let forward: Vec<usize> = (0..chans.len()).collect();
                let reverse: Vec<usize> = forward.iter().rev().copied().collect();
                for order in [forward, reverse] {
                    let got = read_interleaved(&part, &chans, &order)
                        .unwrap_or_else(|e| panic!("{}: {e}", what("interleaved")));
                    for (c, got) in chans.iter().zip(&got) {
                        assert!(*got == c.data, "{}", what("interleaved"));
                    }
                    reads += 1;
                }
            }

            let dw = h.data_window();
            let (lo, hi) = (dw.min.y, dw.max.y);
            let third = (hi - lo) / 3;
            for (y1, y2) in [
                (lo, lo),
                (hi, hi),
                (lo + third, hi - third),
                (hi - 1, lo + 1),
            ] {
                let (a, b) = (y1.min(y2), y1.max(y2));
                if a < lo || b > hi {
                    continue;
                }
                let got = read_lines(&part, &chans, y1, y2)
                    .unwrap_or_else(|e| panic!("{}: {e}", what(&format!("lines {y1} - {y2}"))));
                for (c, got) in chans.iter().zip(&got) {
                    assert!(
                        *got == wheel_rows(c, lo, a, b),
                        "{}",
                        what(&format!(
                            "lines {y1} - {y2}, {}",
                            String::from_utf8_lossy(&c.name)
                        ))
                    );
                }
                reads += 1;
            }

            // the same reads on four threads: the same bytes
            let part4 = threaded.part(p).unwrap();
            let planar4 =
                read_planar(&part4).unwrap_or_else(|e| panic!("{}: {e}", what("4 threads")));
            assert!(planar4 == planar, "{}", what("planar, 4 threads"));
            for (y1, y2) in [(lo + third, hi - third), (hi - 1, lo + 1)] {
                let (a, b) = (y1.min(y2), y1.max(y2));
                if a < lo || b > hi {
                    continue;
                }
                let one = read_lines(&part, &chans, y1, y2).unwrap();
                let four = read_lines(&part4, &chans, y1, y2)
                    .unwrap_or_else(|e| panic!("{}: {e}", what("lines, 4 threads")));
                assert!(
                    four == one,
                    "{}",
                    what(&format!("lines {y1} - {y2}, 4 threads"))
                );
                reads += 1;
            }
            parts += 1;
        }
    }
    eprintln!(
        "{parts} parts as the wheel reads them ({reads} reads); {both_refuse} parts neither reads"
    );
    assert!(parts > 100, "too few parts compared");
    let _ = std::fs::remove_dir_all(&scratch);
}
