// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Fast tier: what the fuzzer (`examples/fuzz.rs`, `tests/heavy_fuzz.rs`) found, kept. A header
//! may ask for any size: a read must fail, not abort; a file that fails part-way through
//! writing stays failed.

use std::path::Path;

use openexr::{ErrorCode, Image, ReadOptions};

/// `comp_none.exr` with its data window's corner moved to `(max_x, max_y)`.
fn vast(max_x: i32, max_y: i32) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../upstream/openexr/src/test/OpenEXRTest/comp_none.exr");
    let mut b = std::fs::read(path).unwrap();
    let key = b"dataWindow\0box2i\0";
    let at = b
        .windows(key.len())
        .position(|w| w == key)
        .expect("a dataWindow attribute")
        + key.len()
        + 4;
    b[at + 8..at + 12].copy_from_slice(&max_x.to_le_bytes());
    b[at + 12..at + 16].copy_from_slice(&max_y.to_le_bytes());
    b
}

#[test]
fn a_vast_data_window_is_an_error_not_an_abort() {
    let file = ReadOptions::new()
        .open_bytes("vast.exr", vast(1_000_000_000, 1_000_000_000))
        .expect("Core opens the header: only its size is wrong");
    let e = Image::from_file(&file).unwrap_err();
    assert_eq!(e.code(), ErrorCode::OutOfMemory.code(), "{e}");
}

#[test]
fn a_size_limit_refuses_the_header() {
    let e = ReadOptions::new()
        .max_image_size(8192, 8192)
        .open_bytes("vast.exr", vast(100_000, 50))
        .unwrap_err();
    assert_ne!(e.code(), ErrorCode::Success.code(), "{e}");
    // within the limit, the same file opens
    ReadOptions::new()
        .max_image_size(8192, 8192)
        .open_bytes("ok.exr", vast(6000, 50))
        .expect("within the limit");
}

#[test]
fn a_write_that_fails_part_way_leaves_the_file_failed() {
    use openexr::{Box2i, Channel, Compression, FrameBuffer, Header, PixelType, V2, WriteOptions};
    // LJ2K takes no sampling by 3 (OpenJPH: powers of 2 only); the first chunk fails
    let dw = Box2i {
        min: V2 { x: 0, y: 0 },
        max: V2 { x: 11, y: 11 },
    };
    let mut h = Header::new(dw);
    let mut c = Channel::new("Y", PixelType::Half);
    c.y_sampling = 3;
    h.insert_channel(c);
    h.set_compression(Compression::Lj2k);
    let mut out = WriteOptions::new()
        .create_writer("m", std::io::Cursor::new(Vec::new()), &[h])
        .unwrap();
    let first = out
        .part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 12)
        .unwrap_err();
    // every later write, and the end, give that error back
    let again = out
        .part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 1)
        .unwrap_err();
    assert_eq!(again, first);
    assert_eq!(out.finish().unwrap_err(), first);
}
