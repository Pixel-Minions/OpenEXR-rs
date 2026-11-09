// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, §16 M4.2): every deep part read through the API gives the wheel's
//! sample counts and samples, bit for bit.
//!
//! The wheel reads a deep part's counts, then its samples, into one array of samples per
//! pixel (PyOpenEXR.cpp:745-875 @ v3.5.3; `oracle/dump_pixels.py`, which writes them pixel
//! after pixel). The images are deep files the wheel writes with each compression a deep part
//! may have (`oracle/write_deep.py`), the deep files of upstream's test tree, and the decode
//! set (the corpus's deep files among them, and damaged copies). Each deep part is read here
//! whole, counts then samples in each channel's own type, and a deep scanline part also by a
//! range of lines; a part the wheel cannot read must fail through the API too, and the
//! reverse.
//!
//! `Image::read` is checked on every file of both sets: where the wheel reads every part
//! that has channels, it gives the wheel's channels (and a deep part's sample counts);
//! where the wheel leaves a part out, it fails.

mod common;

use common::{WheelChannel, deep_images, images, oracle, root, wheel_pixels, write_list};
use openexr::{DeepFrameBuffer, Image, InputPart, MultiPartInputFile, Storage};

/// The part's counts and its channels' samples (in the header's order), lines `y1..=y2` of
/// a deep scanline part (all its tiles at level 0 for a deep tiled part), into buffers over
/// the whole data window.
fn read_deep(part: &InputPart<'_>, y1: i32, y2: i32) -> openexr::Result<(Vec<u32>, Vec<Vec<u8>>)> {
    let h = part.header();
    let dw = h.data_window();
    let mut counts = vec![0u32; (dw.width() * dw.height()) as usize];
    let tiled = h.storage() == Storage::DeepTiled;
    let (nx, ny) = if tiled {
        part.num_tiles((0, 0))?
    } else {
        (0, 0)
    };
    if tiled {
        part.read_tile_sample_counts(&mut counts, dw, (0, nx - 1), (0, ny - 1), (0, 0))?;
    } else {
        part.read_sample_counts(&mut counts, dw, y1, y2)?;
    }
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    let mut bufs: Vec<Vec<u8>> = h
        .channels()
        .iter()
        .map(|c| vec![0u8; total as usize * c.pixel_type.size()])
        .collect();
    let mut fb = DeepFrameBuffer::new(&counts, dw)?;
    for (c, b) in h.channels().iter().zip(bufs.iter_mut()) {
        fb.insert_bytes(&c.name, c.pixel_type, b)?;
    }
    if tiled {
        part.read_deep_tiles(&mut fb, (0, nx - 1), (0, ny - 1), (0, 0))?;
    } else {
        part.read_deep_pixels(&mut fb, y1, y2)?;
    }
    drop(fb);
    Ok((counts, bufs))
}

/// The wheel's counts with only lines `y1..=y2` kept (the rest 0), and each channel's samples
/// of those lines.
fn wheel_lines(chans: &[&WheelChannel], min_y: i32, y1: i32, y2: i32) -> (Vec<u32>, Vec<Vec<u8>>) {
    let all: Vec<u32> = u32s(chans[0].counts.as_ref().expect("deep"));
    let w = chans[0].width;
    let (r1, r2) = ((y1 - min_y) as usize, (y2 - min_y) as usize);
    let counts: Vec<u32> = all
        .iter()
        .enumerate()
        .map(|(i, &c)| if (r1..=r2).contains(&(i / w)) { c } else { 0 })
        .collect();
    let before: u64 = all[..r1 * w].iter().map(|&c| u64::from(c)).sum();
    let within: u64 = all[r1 * w..(r2 + 1) * w]
        .iter()
        .map(|&c| u64::from(c))
        .sum();
    let samples = chans
        .iter()
        .map(|c| {
            let size = c.data.len() / all.iter().map(|&n| n as usize).sum::<usize>().max(1);
            c.data[before as usize * size..(before + within) as usize * size].to_vec()
        })
        .collect();
    (counts, samples)
}

#[test]
fn deep_parts_read_as_the_wheel_reads_them() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_api_deep_{}", std::process::id()));
    let mut all = deep_images(&scratch);
    all.extend(images(&scratch));
    let list = scratch.join("images.txt");
    write_list(&list, &all);
    let out = scratch.join("pixels");
    std::fs::create_dir_all(&out).unwrap();
    let dump = root().join("oracle/dump_pixels.py");
    oracle(&[
        dump.as_os_str(),
        "--many".as_ref(),
        list.as_os_str(),
        out.as_os_str(),
    ]);

    let (mut parts, mut ranges, mut both_refuse) = (0, 0, 0);
    let (mut images_read, mut images_refused) = (0, 0);
    for (n, image) in all.iter().enumerate() {
        let Ok(wheel) = wheel_pixels(&out.join(n.to_string())) else {
            continue; // the wheel refuses the file: the headers check compares refusals
        };
        let file = MultiPartInputFile::open(image).unwrap_or_else(|e| {
            panic!(
                "{}: the wheel reads it, the API refuses it: {e}",
                image.display()
            )
        });
        // Image::read: every part, or the part the wheel left out
        let image_read = Image::from_file(&file);
        let wheel_all = file
            .headers()
            .iter()
            .enumerate()
            .all(|(p, h)| h.channels().is_empty() || wheel.iter().any(|w| w.part == p));
        match (wheel_all, image_read) {
            (true, Ok(img)) => {
                for (p, ip) in img.parts.iter().enumerate() {
                    for ch in &ip.channels {
                        let w = wheel
                            .iter()
                            .find(|w| w.part == p && w.name == ch.name)
                            .unwrap_or_else(|| panic!("{}: part {p}: a channel", image.display()));
                        assert!(
                            ch.pixels.as_bytes() == w.data.as_slice(),
                            "{}: Image::read, part {p}, {}",
                            image.display(),
                            String::from_utf8_lossy(&ch.name)
                        );
                        assert_eq!(
                            (ch.width, ch.height),
                            (w.width, w.height),
                            "{}",
                            image.display()
                        );
                        if let Some(counts) = &w.counts {
                            assert!(
                                ip.sample_counts.as_ref() == Some(&u32s(counts)),
                                "{}: Image::read, part {p}: sample counts",
                                image.display()
                            );
                        }
                    }
                }
                images_read += 1;
            }
            (true, Err(e)) => panic!(
                "{}: the wheel reads every part, Image::read fails: {e}",
                image.display()
            ),
            (false, Ok(_)) => panic!(
                "{}: the wheel leaves a part out, Image::read reads it",
                image.display()
            ),
            (false, Err(_)) => images_refused += 1,
        }

        for (p, h) in file.headers().iter().enumerate() {
            if !h.storage().is_deep() || h.channels().is_empty() {
                continue;
            }
            let part = file.part(p).unwrap();
            let what = |how: &str| format!("{}: part {p}, {how}", image.display());
            let dw = h.data_window();
            let whole = read_deep(&part, dw.min.y, dw.max.y);
            let chans: Vec<&WheelChannel> = h
                .channels()
                .iter()
                .filter_map(|c| wheel.iter().find(|w| w.part == p && w.name == c.name))
                .collect();
            if chans.is_empty() {
                assert!(
                    whole.is_err(),
                    "{}",
                    what("the wheel cannot read it, the API reads it")
                );
                both_refuse += 1;
                continue;
            }
            let (counts, samples) = whole.unwrap_or_else(|e| {
                panic!("{}: {e}", what("the wheel reads it, the API refuses it"))
            });
            assert_eq!(chans.len(), h.channels().len(), "{}", what("channels"));
            let want: Vec<u32> = u32s(chans[0].counts.as_ref().unwrap());
            assert!(counts == want, "{}", what("sample counts"));
            for (c, got) in chans.iter().zip(&samples) {
                assert!(
                    *got == c.data,
                    "{}",
                    what(&format!("samples of {}", String::from_utf8_lossy(&c.name)))
                );
            }
            parts += 1;

            if h.storage() == Storage::DeepScanline {
                let third = (dw.max.y - dw.min.y) / 3;
                let (y1, y2) = (dw.min.y + third, dw.max.y - third);
                let (counts, samples) = read_deep(&part, y2, y1)
                    .unwrap_or_else(|e| panic!("{}: {e}", what(&format!("lines {y1} - {y2}"))));
                let (want_counts, want_samples) = wheel_lines(&chans, dw.min.y, y1, y2);
                assert!(
                    counts == want_counts,
                    "{}",
                    what(&format!("counts of lines {y1} - {y2}"))
                );
                for ((c, got), want) in chans.iter().zip(&samples).zip(&want_samples) {
                    assert!(
                        got == want,
                        "{}",
                        what(&format!(
                            "lines {y1} - {y2}, {}",
                            String::from_utf8_lossy(&c.name)
                        ))
                    );
                }
                ranges += 1;
            }
        }
    }
    eprintln!(
        "{parts} deep parts as the wheel reads them ({ranges} line ranges); {both_refuse} parts neither reads"
    );
    eprintln!(
        "Image::read: {images_read} files as the wheel reads them; {images_refused} refused as the wheel leaves a part out"
    );
    assert!(parts >= 24, "too few deep parts compared");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Native-order `uint32` bytes as values.
fn u32s(b: &[u8]) -> Vec<u32> {
    b.as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_ne_bytes(*c))
        .collect()
}
