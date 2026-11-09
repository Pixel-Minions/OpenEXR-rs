// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Fast tier: the API's deep reads of upstream's deep test images (src/test/bin/test_images):
//! its own contract (fill values, the counts the samples must match, windows and buffers that
//! are too small) and its paths against each other (a converted read against the file's type,
//! line ranges against the whole, `Image::read` against the frame buffers). The samples
//! themselves are checked against the wheel in the heavy tier (`heavy_deep.rs`).

use std::path::{Path, PathBuf};

use openexr::{DeepFrameBuffer, Image, MultiPartInputFile, PixelType, Pixels, half};

fn image(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream/openexr/src/test/bin/test_images")
        .join(name)
}

const IMAGES: [&str; 3] = ["11.deep.exr", "42.deep.exr", "objectid.deep.exr"];

fn whole_counts(file: &MultiPartInputFile) -> Vec<u32> {
    let part = file.part(0).unwrap();
    let dw = part.header().data_window();
    let mut counts = vec![0u32; (dw.width() * dw.height()) as usize];
    part.read_sample_counts(&mut counts, dw, dw.min.y, dw.max.y)
        .unwrap();
    counts
}

#[test]
fn a_missing_channel_gets_its_fill_value_in_every_sample() {
    for name in IMAGES {
        let file = MultiPartInputFile::open(image(name)).unwrap();
        let part = file.part(0).unwrap();
        let dw = part.header().data_window();
        let counts = whole_counts(&file);
        let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
        assert!(total > 0, "{name}");
        let mut floats = vec![0f32; total as usize];
        let mut halves = vec![0u16; total as usize];
        let mut fb = DeepFrameBuffer::new(&counts, dw).unwrap();
        fb.insert("no such float", &mut floats).unwrap().fill_value = 0.25;
        fb.insert("no such half", &mut halves).unwrap().fill_value = 0.5;
        part.read_deep_pixels(&mut fb, dw.min.y, dw.max.y).unwrap();
        drop(fb);
        assert!(floats.iter().all(|&f| f == 0.25), "{name}");
        assert!(halves.iter().all(|&h| h == half::from_f32(0.5)), "{name}");
    }
}

#[test]
fn halves_read_as_floats_are_cores_conversion_of_them() {
    for name in IMAGES {
        let file = MultiPartInputFile::open(image(name)).unwrap();
        let part = file.part(0).unwrap();
        let h = part.header();
        let dw = h.data_window();
        let counts = whole_counts(&file);
        let total = counts.iter().map(|&c| c as usize).sum::<usize>();
        let c = h
            .channels()
            .iter()
            .find(|c| c.pixel_type == PixelType::Half)
            .unwrap();
        let mut halves = vec![0u16; total];
        let mut floats = vec![0f32; total];
        for read_as_float in [false, true] {
            let mut fb = DeepFrameBuffer::new(&counts, dw).unwrap();
            if read_as_float {
                fb.insert(&c.name, &mut floats).unwrap();
            } else {
                fb.insert(&c.name, &mut halves).unwrap();
            }
            part.read_deep_pixels(&mut fb, dw.min.y, dw.max.y).unwrap();
        }
        for (f, h) in floats.iter().zip(&halves) {
            assert_eq!(f.to_bits(), half::to_f32(*h).to_bits(), "{name}");
        }
    }
}

#[test]
fn line_ranges_read_as_the_whole_does() {
    let file = MultiPartInputFile::open(image("42.deep.exr")).unwrap();
    let whole = Image::from_file(&file).unwrap();
    let part = file.part(0).unwrap();
    let h = part.header();
    let dw = h.data_window();
    let all = whole.parts[0].sample_counts.as_ref().unwrap();
    let w = dw.width() as usize;
    for (y1, y2) in [
        (dw.min.y, dw.min.y),
        (dw.min.y + 7, dw.min.y + 100),
        (dw.max.y, dw.max.y),
    ] {
        let mut counts = vec![0u32; all.len()];
        part.read_sample_counts(&mut counts, dw, y2, y1).unwrap();
        let rows = (y1 - dw.min.y) as usize..=(y2 - dw.min.y) as usize;
        for (i, (&got, &want)) in counts.iter().zip(all).enumerate() {
            assert_eq!(got, if rows.contains(&(i / w)) { want } else { 0 });
        }
        let total: usize = counts.iter().map(|&c| c as usize).sum();
        let before: usize = all[..rows.start() * w].iter().map(|&c| c as usize).sum();
        for (c, ch) in h.channels().iter().zip(&whole.parts[0].channels) {
            let size = c.pixel_type.size();
            let mut buf = vec![0u8; total * size];
            let mut fb = DeepFrameBuffer::new(&counts, dw).unwrap();
            fb.insert_bytes(&c.name, c.pixel_type, &mut buf).unwrap();
            part.read_deep_pixels(&mut fb, y1, y2).unwrap();
            drop(fb);
            assert_eq!(
                buf,
                ch.pixels.as_bytes()[before * size..(before + total) * size],
                "lines {y1} - {y2}"
            );
        }
    }
}

#[test]
fn samples_need_the_parts_own_counts() {
    let file = MultiPartInputFile::open(image("11.deep.exr")).unwrap();
    let part = file.part(0).unwrap();
    let h = part.header();
    let dw = h.data_window();
    let mut counts = whole_counts(&file);
    // one sample moved from a pixel to another: the same total, other places
    let from = counts.iter().position(|&c| c > 0).unwrap();
    counts[from] -= 1;
    let next = (from + 1) % counts.len();
    counts[next] += 1;
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let mut buf = vec![0u16; total];
    let mut fb = DeepFrameBuffer::new(&counts, dw).unwrap();
    fb.insert(&h.channels()[0].name, &mut buf).unwrap();
    assert!(part.read_deep_pixels(&mut fb, dw.min.y, dw.max.y).is_err());
}

#[test]
fn small_buffers_and_windows_are_refused() {
    let file = MultiPartInputFile::open(image("64.deep.exr")).unwrap();
    let part = file.part(0).unwrap();
    let dw = part.header().data_window();
    let counts = whole_counts(&file);
    // counts shorter than the window
    assert!(DeepFrameBuffer::new(&counts[..counts.len() - 1], dw).is_err());
    let mut short = vec![0u32; counts.len() - 1];
    assert!(
        part.read_sample_counts(&mut short, dw, dw.min.y, dw.max.y)
            .is_err()
    );
    // a window without the lines read
    let mut small = dw;
    small.max.y -= 1;
    let mut c2 = vec![0u32; (small.width() * small.height()) as usize];
    assert!(
        part.read_sample_counts(&mut c2, small, dw.min.y, dw.max.y)
            .is_err()
    );
    // samples shorter than the counts' total
    let total: usize = counts.iter().map(|&c| c as usize).sum();
    let mut buf = vec![0u16; total - 1];
    let mut fb = DeepFrameBuffer::new(&counts, dw).unwrap();
    assert!(fb.insert("A", &mut buf).is_err());
    // flat reads of a deep part
    assert!(
        part.read_pixels(&mut openexr::FrameBuffer::new(), dw.min.y, dw.max.y)
            .is_err()
    );
}

#[test]
fn image_read_gives_typed_channels() {
    let img = Image::read(image("objectid.deep.exr")).unwrap();
    let p = &img.parts[0];
    let total: usize = p
        .sample_counts
        .as_ref()
        .unwrap()
        .iter()
        .map(|&c| c as usize)
        .sum();
    for (ch, c) in p.channels.iter().zip(p.header.channels()) {
        assert_eq!(ch.pixels.len(), total);
        assert_eq!(ch.pixels.pixel_type(), c.pixel_type);
        assert!(matches!(ch.pixels, Pixels::Half(_) | Pixels::Uint(_)));
    }
}
