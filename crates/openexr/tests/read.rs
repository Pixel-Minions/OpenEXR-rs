// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Fast tier: the API's reads of upstream's test images. Its own contract: fill values,
//! refusals before anything is written, sampling checks; and its paths against each other
//! (tiles against lines, a converted read against the file's type). The pixels themselves are
//! checked against the wheel in the heavy tier (`heavy_read.rs`).

use std::path::{Path, PathBuf};

use openexr::{FrameBuffer, MultiPartInputFile, PixelType, Slice, half};

fn image(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream/openexr/src/test/OpenEXRTest")
        .join(name)
}

/// Every channel of part 0 read whole, in its own type, as bytes.
fn read_all(file: &MultiPartInputFile, y1: i32, y2: i32, fill: u8) -> Vec<Vec<u8>> {
    let part = file.part(0).unwrap();
    let h = part.header();
    let dw = h.data_window();
    // (upstream's sampled images start their data windows at 0)
    let dims = |c: &openexr::Channel| {
        (
            (dw.width() as usize).div_ceil(c.x_sampling as usize),
            (dw.height() as usize).div_ceil(c.y_sampling as usize),
        )
    };
    let mut bufs: Vec<Vec<u8>> = h
        .channels()
        .iter()
        .map(|c| vec![fill; dims(c).0 * dims(c).1 * c.pixel_type.size()])
        .collect();
    let mut fb = FrameBuffer::new();
    for (c, b) in h.channels().iter().zip(bufs.iter_mut()) {
        let size = c.pixel_type.size() as i64;
        let w = dims(c).0;
        let id = fb.add_bytes(b);
        let mut s = Slice::new(c.pixel_type, id, 0, dw.min, size, size * w as i64);
        s.x_sampling = c.x_sampling;
        s.y_sampling = c.y_sampling;
        fb.insert(&c.name, s).unwrap();
    }
    part.read_pixels(&mut fb, y1, y2).unwrap();
    drop(fb);
    bufs
}

#[test]
fn a_missing_channel_gets_its_fill_value() {
    for name in ["comp_none.exr", "comp_zip.exr", "v1.7.test.tiled.exr"] {
        let file = MultiPartInputFile::open(image(name)).unwrap();
        let part = file.part(0).unwrap();
        let dw = part.header().data_window();
        let n = (dw.width() * dw.height()) as usize;
        // whole, then lines 1 to 2 (a tiled part's buffered path)
        for (y1, y2) in [(dw.min.y, dw.max.y), (dw.min.y + 1, dw.min.y + 2)] {
            let mut halves = vec![0u16; n];
            let mut floats = vec![0f32; n];
            let mut uints = vec![0u32; n];
            let mut fb = FrameBuffer::new();
            fb.insert_plane("no such half", &mut halves, dw)
                .unwrap()
                .fill_value = 0.5;
            fb.insert_plane("no such float", &mut floats, dw)
                .unwrap()
                .fill_value = 0.25;
            fb.insert_plane("no such uint", &mut uints, dw)
                .unwrap()
                .fill_value = 7.0;
            part.read_pixels(&mut fb, y1, y2).unwrap();
            drop(fb);
            let w = dw.width() as usize;
            for (i, ((h, f), u)) in halves.iter().zip(&floats).zip(&uints).enumerate() {
                let y = dw.min.y + (i / w) as i32;
                if (y1..=y2).contains(&y) {
                    assert_eq!(*h, half::from_f32(0.5), "{name}");
                    assert_eq!(*f, 0.25, "{name}");
                    assert_eq!(*u, 7, "{name}");
                } else {
                    assert_eq!((*h, *f, *u), (0, 0.0, 0), "{name}: line {y} was written");
                }
            }
        }
    }
}

#[test]
fn a_slice_outside_its_buffer_is_refused_before_anything_is_written() {
    for name in ["comp_none.exr", "v1.7.test.tiled.exr"] {
        let file = MultiPartInputFile::open(image(name)).unwrap();
        let part = file.part(0).unwrap();
        let h = part.header();
        let dw = h.data_window();
        let c = &h.channels()[0];
        let size = c.pixel_type.size();
        let w = dw.width() as usize;
        // a plane one sample short
        let n = w * dw.height() as usize - 1;
        let mut short = vec![0u8; n * size];
        let mut fb = FrameBuffer::new();
        let id = fb.add_bytes(&mut short);
        let s = Slice::new(c.pixel_type, id, 0, dw.min, size as i64, (size * w) as i64);
        fb.insert(&c.name, s).unwrap();
        let e = part.read_pixels(&mut fb, dw.min.y, dw.max.y).unwrap_err();
        assert_eq!(
            e.code(),
            openexr::ErrorCode::InvalidArgument.code(),
            "{name}"
        );
        drop(fb);
        assert!(
            short.iter().all(|&b| b == 0),
            "{name}: written before the refusal"
        );

        let mut too_short = vec![0f32; n];
        let mut fb = FrameBuffer::new();
        assert!(fb.insert_plane(&c.name, &mut too_short, dw).is_err());
    }
}

#[test]
fn lines_outside_the_data_window_are_refused() {
    let file = MultiPartInputFile::open(image("comp_none.exr")).unwrap();
    let part = file.part(0).unwrap();
    let dw = part.header().data_window();
    let mut fb = FrameBuffer::new();
    assert!(part.read_pixels(&mut fb, dw.min.y - 1, dw.min.y).is_err());
    assert!(part.read_pixels(&mut fb, dw.max.y, dw.max.y + 1).is_err());
    // either order
    part.read_pixels(&mut fb, dw.max.y, dw.min.y).unwrap();
}

#[test]
fn a_slice_must_be_sampled_as_its_channel() {
    let file = MultiPartInputFile::open(image("comp_b44.exr")).unwrap();
    let part = file.part(0).unwrap();
    let h = part.header();
    let dw = h.data_window();
    let c = h
        .channels()
        .iter()
        .find(|c| c.x_sampling > 1)
        .expect("comp_b44.exr has sampled channels");
    let mut plane = vec![0u16; (dw.width() * dw.height()) as usize];
    let mut fb = FrameBuffer::new();
    fb.insert_plane(&c.name, &mut plane, dw).unwrap();
    assert!(part.read_pixels(&mut fb, dw.min.y, dw.max.y).is_err());
}

#[test]
fn halves_read_as_floats_are_cores_conversion_of_them() {
    for name in ["comp_piz.exr", "v1.7.test.tiled.exr", "comp_b44.exr"] {
        let file = MultiPartInputFile::open(image(name)).unwrap();
        let part = file.part(0).unwrap();
        let h = part.header();
        let dw = h.data_window();
        let native = read_all(&file, dw.min.y, dw.max.y, 0);
        for (c, native) in h.channels().iter().zip(&native) {
            if c.pixel_type != PixelType::Half {
                continue;
            }
            let w = (dw.width() as usize).div_ceil(c.x_sampling as usize);
            let rows = (dw.height() as usize).div_ceil(c.y_sampling as usize);
            let mut floats = vec![0f32; w * rows];
            let mut fb = FrameBuffer::new();
            fb.insert_sampled_plane(&c.name, &mut floats, dw, c.x_sampling, c.y_sampling)
                .unwrap();
            part.read_pixels(&mut fb, dw.min.y, dw.max.y).unwrap();
            drop(fb);
            let halves: &[u16] = bytemuck::cast_slice(&native[..w * rows * 2]);
            for (f, h) in floats.iter().zip(halves) {
                assert_eq!(f.to_bits(), half::to_f32(*h).to_bits(), "{name}");
            }
        }
    }
}

#[test]
fn a_tiled_part_reads_alike_by_tiles_and_by_lines() {
    let file = MultiPartInputFile::open(image("v1.7.test.tiled.exr")).unwrap();
    let part = file.part(0).unwrap();
    let h = part.header();
    let dw = h.data_window();
    assert_eq!(part.num_levels().unwrap(), (1, 1));
    assert!(part.is_valid_level(0, 0));
    assert!(!part.is_valid_level(1, 0));
    let by_lines = read_all(&file, dw.min.y, dw.max.y, 0);
    // line by line, through the buffered rows of tiles
    let mut line_by_line: Vec<Vec<u8>> = by_lines.iter().map(|b| vec![0u8; b.len()]).collect();
    for y in dw.min.y..=dw.max.y {
        let one = read_all(&file, y, y, 0);
        for (acc, one) in line_by_line.iter_mut().zip(&one) {
            let row = acc.len() / dw.height() as usize;
            let r = (y - dw.min.y) as usize * row;
            acc[r..r + row].copy_from_slice(&one[r..r + row]);
        }
    }
    assert_eq!(line_by_line, by_lines);
    let (nx, ny) = part.num_tiles((0, 0)).unwrap();
    let w = dw.width() as usize;
    let mut by_tiles: Vec<Vec<u8>> = by_lines.iter().map(|b| vec![0u8; b.len()]).collect();
    let mut fb = FrameBuffer::new();
    for (c, b) in h.channels().iter().zip(by_tiles.iter_mut()) {
        let size = c.pixel_type.size() as i64;
        let id = fb.add_bytes(b);
        fb.insert(
            &c.name,
            Slice::new(c.pixel_type, id, 0, dw.min, size, size * w as i64),
        )
        .unwrap();
    }
    // tile by tile, in reverse
    for ty in (0..ny).rev() {
        for tx in (0..nx).rev() {
            part.read_tile(&mut fb, tx, ty, (0, 0)).unwrap();
        }
    }
    drop(fb);
    assert_eq!(by_tiles, by_lines);
}
