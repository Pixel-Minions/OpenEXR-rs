// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, §16 M4.1): what the wheel cannot show, against Core 3.5.3 itself
//! (the source-built harness, PLAN.md O1): every level of every tiled part, and every flat
//! part read as half, float and uint, through the API's `read_pixels` (a scanline part) and
//! `read_tiles` (each level of a tiled part), give the bytes `core_harness levels` gives,
//! and fail where it fails. Level 0 in each channel's own type is the wheel's
//! (`heavy_read.rs`), which ties the harness to the wheel.
//!
//! The harness is built by `scripts/oracle-native-build.cmd` (Windows) or
//! `scripts/oracle-native-build.sh` (Linux).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{images, root, unhex};
use openexr::{FrameBuffer, InputPart, MultiPartInputFile, PixelType, Slice, Storage};

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

/// A level of a part in a type: `(part, lx, ly, type)`.
type Key = (usize, i32, i32, String);

/// What the harness decoded: per level and type, its channels' names and bytes, or the
/// error.
type Levels = BTreeMap<Key, Result<Vec<(Vec<u8>, Vec<u8>)>, String>>;

fn harness_levels(image: &Path, out: &Path) -> Option<Levels> {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out).unwrap();
    let status = Command::new(harness())
        .arg("levels")
        .arg(image)
        .arg(out)
        .status()
        .expect("the harness runs");
    assert!(status.success(), "core_harness levels {}", image.display());
    let manifest = std::fs::read_to_string(out.join("manifest.tsv")).unwrap();
    let mut levels = Levels::new();
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] == "error" {
            return None;
        }
        let key = (
            cols[0].parse().unwrap(),
            cols[1].parse().unwrap(),
            cols[2].parse().unwrap(),
            cols[3].to_owned(),
        );
        if cols[4] == "error" {
            levels.insert(key, Err(cols[5].to_owned()));
            continue;
        }
        let data = std::fs::read(out.join(cols[8])).unwrap();
        levels
            .entry(key)
            .or_insert_with(|| Ok(Vec::new()))
            .as_mut()
            .expect("a level's channels")
            .push((unhex(cols[5]), data));
    }
    Some(levels)
}

fn pixel_type(name: &str) -> PixelType {
    match name {
        "HALF" => PixelType::Half,
        "FLOAT" => PixelType::Float,
        _ => PixelType::Uint,
    }
}

/// How many of `lo..=hi` are multiples of `s`.
fn samples(lo: i32, hi: i32, s: i32) -> usize {
    (lo..=hi).filter(|v| v % s == 0).count()
}

/// Level `(lx, ly)` of the part read through the API as type `t`, one plane per channel.
fn api_level(
    part: &InputPart<'_>,
    lx: i32,
    ly: i32,
    t: PixelType,
) -> openexr::Result<Vec<Vec<u8>>> {
    let h = part.header();
    let tiled = h.storage() == Storage::Tiled;
    let window = if tiled {
        part.data_window_for_level((lx, ly))?
    } else {
        h.data_window()
    };
    let size = t.size();
    let dims: Vec<(usize, usize)> = h
        .channels()
        .iter()
        .map(|c| {
            (
                samples(window.min.x, window.max.x, c.x_sampling),
                samples(window.min.y, window.max.y, c.y_sampling),
            )
        })
        .collect();
    let mut planes: Vec<Vec<u8>> = dims.iter().map(|(w, h)| vec![0u8; w * h * size]).collect();
    let mut fb = FrameBuffer::new();
    for ((c, plane), (w, _)) in h.channels().iter().zip(planes.iter_mut()).zip(&dims) {
        let id = fb.add_bytes(plane);
        let mut s = Slice::new(t, id, 0, window.min, size as i64, (size * w) as i64);
        s.x_sampling = c.x_sampling;
        s.y_sampling = c.y_sampling;
        fb.insert(&c.name, s)?;
    }
    if tiled {
        let (nx, ny) = part.num_tiles((lx, ly))?;
        part.read_tiles(&mut fb, (0, nx - 1), (0, ny - 1), (lx, ly))?;
    } else {
        part.read_pixels(&mut fb, window.min.y, window.max.y)?;
    }
    drop(fb);
    Ok(planes)
}

#[test]
fn levels_and_types_read_as_core_reads_them() {
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_api_levels_{}",
        std::process::id()
    ));
    let (mut compared, mut tiled_levels, mut both_fail) = (0, 0, 0);
    for image in images(&scratch) {
        let out = scratch.join("levels");
        let Some(levels) = harness_levels(&image, &out) else {
            continue; // Core refuses the file: the headers check compares refusals
        };
        let file = MultiPartInputFile::open(&image).unwrap_or_else(|e| {
            panic!(
                "{}: Core opens it, the API refuses it: {e}",
                image.display()
            )
        });
        for ((p, lx, ly, t), want) in &levels {
            let part = file.part(*p).unwrap();
            let what = format!("{}: part {p}, level ({lx}, {ly}), {t}", image.display());
            let got = api_level(&part, *lx, *ly, pixel_type(t));
            match (want, got) {
                (Err(_), Err(_)) => both_fail += 1,
                (Err(e), Ok(_)) => panic!("{what}: Core fails ({e}), the API reads it"),
                (Ok(_), Err(e)) => panic!("{what}: Core reads it, the API fails: {e}"),
                (Ok(want), Ok(got)) => {
                    assert_eq!(want.len(), got.len(), "{what}: channels");
                    for ((name, w), g) in want.iter().zip(&got) {
                        assert!(
                            w == g,
                            "{what}, {}: the API's bytes are not Core's",
                            String::from_utf8_lossy(name)
                        );
                    }
                    compared += 1;
                    if (*lx, *ly) != (0, 0) {
                        tiled_levels += 1;
                    }
                }
            }
        }
    }
    eprintln!(
        "{compared} levels and types as Core's ({tiled_levels} of them above level 0); {both_fail} fail in both"
    );
    assert!(tiled_levels > 0, "no level above 0 compared");
    let _ = std::fs::remove_dir_all(&scratch);
}
