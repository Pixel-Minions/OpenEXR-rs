// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier (PLAN.md §8, §16 M4.3): files written through the API are the wheel's, byte for
//! byte.
//!
//! `oracle/write_images.py` writes images in every compression and many layouts, and keeps
//! the pixels each was written from (`raw/`). Each image's headers are read through the API,
//! its pixels taken from those arrays (a subsampled channel's are the array's first values,
//! as the binding hands them over: PyOpenEXR.cpp:891-987 @ v3.5.3), and the file written
//! through the API as the binding writes it (every line of a scanline part in one call,
//! `writePixels (height)`; every tile of a tiled part's level 0, `writeTiles`) must be the
//! wheel's file.

mod common;

use std::io::Cursor;
use std::path::Path;

use common::{COMPRESSIONS, oracle, root};
use openexr::{FrameBuffer, MultiPartInputFile, Storage, WriteOptions};

/// The file the API writes for `image` from the pixels in `raw/`, or why it cannot.
fn write_from_raw(image: &Path, file: &MultiPartInputFile) -> openexr::Result<Option<Vec<u8>>> {
    let headers = file.headers();
    if headers.iter().any(|h| h.storage().is_deep()) {
        return Ok(None);
    }
    let stem = image.file_stem().unwrap().to_string_lossy().into_owned();
    let raw_dir = image.parent().unwrap().join("raw");
    let mut out =
        WriteOptions::new().create_writer("<memory>", Cursor::new(Vec::new()), headers)?;
    for (p, h) in headers.iter().enumerate() {
        let dw = h.data_window();
        let raws: Vec<Vec<u8>> = h
            .channels()
            .iter()
            .map(|c| {
                let name = String::from_utf8_lossy(&c.name);
                std::fs::read(raw_dir.join(format!("{stem}.{name}.raw"))).expect("the raw pixels")
            })
            .collect();
        let mut fb = FrameBuffer::new();
        for (c, raw) in h.channels().iter().zip(&raws) {
            let w = (dw.width() as usize).div_ceil(c.x_sampling as usize);
            let rows = (dw.height() as usize).div_ceil(c.y_sampling as usize);
            let n = w * rows * c.pixel_type.size();
            let id = fb.add_bytes(&raw[..n]);
            let size = c.pixel_type.size() as i64;
            let mut s = openexr::Slice::new(c.pixel_type, id, 0, dw.min, size, size * w as i64);
            s.x_sampling = c.x_sampling;
            s.y_sampling = c.y_sampling;
            fb.insert(&c.name, s)?;
        }
        let mut part = out.part(p)?;
        if h.storage() == Storage::Tiled {
            // writeTiles (0, numXTiles - 1, 0, numYTiles - 1): level 0
            let (nx, ny) = part.num_tiles((0, 0))?;
            part.write_tiles(&fb, (0, nx - 1), (0, ny - 1), (0, 0))?;
        } else {
            part.write_pixels(&fb, dw.height() as i32)?;
        }
    }
    Ok(Some(out.finish()?.into_inner()))
}

#[test]
fn files_write_as_the_wheel_wrote_them() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_api_write_{}", std::process::id()));
    let written = scratch.join("written");
    let script = root().join("oracle/write_images.py");
    let mut args = vec![script.as_os_str(), written.as_os_str()];
    args.extend(COMPRESSIONS.iter().map(std::ffi::OsStr::new));
    oracle(&args);
    let mut images: Vec<_> = std::fs::read_dir(&written)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "exr"))
        .collect();
    images.sort();

    let mut compared = 0;
    for image in &images {
        let file = MultiPartInputFile::open(image).unwrap();
        let name = image.file_name().unwrap().to_string_lossy().into_owned();
        let Some(ours) = write_from_raw(image, &file).unwrap_or_else(|e| panic!("{name}: {e}"))
        else {
            continue;
        };
        let wheel = std::fs::read(image).unwrap();
        if ours != wheel {
            let at = ours
                .iter()
                .zip(&wheel)
                .position(|(a, b)| a != b)
                .unwrap_or(ours.len().min(wheel.len()));
            panic!(
                "{name}: differs at byte {at} (the API's {} bytes, the wheel's {})",
                ours.len(),
                wheel.len()
            );
        }
        compared += 1;
    }
    eprintln!("{compared} files written as the wheel wrote them");
    assert!(compared > 100, "too few files compared");
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Values for a plane: any bits, from a fixed sequence.
fn values(n: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// Heavy tier: every level of a MIP- or RIP-mapped tiled part, which the wheel never writes
/// (the binding writes level 0), written through the API tile by tile in scrambled order
/// (the tiles kept until their turn, as `bufferedTileWrite` keeps them), in each line order
/// and lossless compression, reads back through Core 3.5.3 itself (`core_harness levels`)
/// as the pixels written.
#[test]
fn levels_written_read_back_through_core() {
    use openexr::{
        Channel, Compression, Header, LevelMode, LineOrder, PixelType, RoundingMode,
        TileDescription, V2,
    };
    let scratch = std::env::temp_dir().join(format!(
        "openexr_rs_heavy_api_levels_write_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    let dw = openexr::Box2i {
        min: V2 { x: -3, y: 5 },
        max: V2 { x: 57, y: 41 },
    };
    let chans = [
        ("A", PixelType::Half),
        ("B", PixelType::Float),
        ("C", PixelType::Uint),
    ];
    let mut checked = 0;
    let mut seed = 7;
    for mode in [LevelMode::Mipmap, LevelMode::Ripmap] {
        for (order, rounding) in [
            (LineOrder::IncreasingY, RoundingMode::Down),
            (LineOrder::DecreasingY, RoundingMode::Up),
            (LineOrder::RandomY, RoundingMode::Down),
        ] {
            for comp in [
                Compression::None,
                Compression::Rle,
                Compression::Zip,
                Compression::Piz,
                Compression::Zstd,
            ] {
                let mut h = Header::new(dw);
                for (n, t) in chans {
                    h.insert_channel(Channel::new(n, t));
                }
                h.set_compression(comp);
                h.set_line_order(order);
                h.set_tiles(TileDescription {
                    x_size: 16,
                    y_size: 8,
                    level_mode: mode,
                    rounding_mode: rounding,
                });
                let mut out = WriteOptions::new()
                    .create_writer(
                        "<memory>",
                        Cursor::new(Vec::new()),
                        std::slice::from_ref(&h),
                    )
                    .unwrap();
                let mut part = out.part(0).unwrap();
                let levels: Vec<(i32, i32)> = (0..32)
                    .flat_map(|ly| (0..32).map(move |lx| (lx, ly)))
                    .filter(|&l| part.num_tiles(l).is_ok())
                    .collect();
                // each level's planes, then its tiles from the last to the first, and the
                // levels from the last to the first
                let mut planes = std::collections::BTreeMap::new();
                for &level in levels.iter().rev() {
                    let (w, ht) = part.level_size(level).unwrap();
                    let pl: Vec<Vec<u8>> = chans
                        .iter()
                        .map(|(_, t)| {
                            seed += 1;
                            values(w as usize * ht as usize * t.size(), seed)
                        })
                        .collect();
                    let mut fb = FrameBuffer::new();
                    for ((n, t), b) in chans.iter().zip(&pl) {
                        let id = fb.add_bytes(&b[..]);
                        let size = t.size() as i64;
                        fb.insert(
                            n,
                            openexr::Slice::new(*t, id, 0, dw.min, size, size * w as i64),
                        )
                        .unwrap();
                    }
                    let (nx, ny) = part.num_tiles(level).unwrap();
                    for ty in (0..ny).rev() {
                        for tx in (0..nx).rev() {
                            part.write_tile(&fb, tx, ty, level).unwrap();
                        }
                    }
                    drop(fb);
                    planes.insert(level, pl);
                }
                let bytes = out.finish().unwrap().into_inner();
                let file = scratch.join(format!("{checked}.exr"));
                std::fs::write(&file, &bytes).unwrap();
                let dump = scratch.join(format!("{checked}"));
                std::fs::create_dir_all(&dump).unwrap();
                let status = std::process::Command::new(common::harness())
                    .arg("levels")
                    .arg(&file)
                    .arg(&dump)
                    .status()
                    .unwrap();
                assert!(status.success());
                let manifest = std::fs::read_to_string(dump.join("manifest.tsv")).unwrap();
                let mut seen = 0;
                for line in manifest.lines().filter(|l| !l.is_empty()) {
                    let cols: Vec<&str> = line.split('\t').collect();
                    let what = format!("{mode:?} {order:?} {comp:?}: {line}");
                    assert_ne!(cols[4], "error", "{what}: Core cannot read the API's file");
                    let level = (cols[1].parse().unwrap(), cols[2].parse().unwrap());
                    let c: usize = cols[4].parse().unwrap();
                    let native = match chans[c].1 {
                        PixelType::Half => "HALF",
                        PixelType::Float => "FLOAT",
                        _ => "UINT",
                    };
                    if cols[3] != native {
                        continue;
                    }
                    let got = std::fs::read(dump.join(cols[8])).unwrap();
                    assert!(got == planes[&level][c], "{what}: not the pixels written");
                    seen += 1;
                }
                assert_eq!(
                    seen,
                    levels.len() * chans.len(),
                    "{mode:?} {order:?} {comp:?}"
                );
                checked += 1;
            }
        }
    }
    eprintln!("{checked} multi-level files read back through Core as written");
    let _ = std::fs::remove_dir_all(&scratch);
}
