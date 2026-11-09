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
//! `writePixels (height)`) must be the wheel's file.

mod common;

use std::io::Cursor;
use std::path::Path;

use common::{COMPRESSIONS, oracle, root};
use openexr::{FrameBuffer, MultiPartInputFile, Storage, WriteOptions};

/// The file the API writes for `image` from the pixels in `raw/`, or why it cannot.
fn write_from_raw(image: &Path, file: &MultiPartInputFile) -> openexr::Result<Option<Vec<u8>>> {
    let headers = file.headers();
    if headers.iter().any(|h| h.storage() != Storage::Scanline) {
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
        out.part(p)?.write_pixels(&fb, dw.height() as i32)?;
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
