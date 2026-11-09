// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Fast tier: the API's writer's own contract (round trips through a lossless compression,
//! refusals, incomplete parts, part order). The bytes themselves are checked against the
//! wheel in the heavy tier (`heavy_write.rs`).

use std::io::Cursor;

use openexr::{
    Box2i, Channel, Compression, FrameBuffer, Header, LineOrder, MultiPartInputFile, PixelType,
    ReadOptions, V2, WriteOptions,
};

fn window(w: i32, h: i32) -> Box2i {
    Box2i {
        min: V2 { x: -3, y: 5 },
        max: V2 {
            x: -3 + w - 1,
            y: 5 + h - 1,
        },
    }
}

fn header(dw: Box2i, compression: Compression, order: LineOrder) -> Header {
    let mut h = Header::new(dw);
    h.insert_channel(Channel::new("Y", PixelType::Half));
    h.insert_channel(Channel::new("Z", PixelType::Float));
    h.insert_channel(Channel::new("id", PixelType::Uint));
    h.set_compression(compression);
    h.set_line_order(order);
    h
}

/// Values for a plane: any bits, from a fixed sequence.
fn values(n: usize, seed: u32) -> Vec<u32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            x
        })
        .collect()
}

#[test]
fn lossless_files_read_back_as_written() {
    let dw = window(37, 41);
    let n = (dw.width() * dw.height()) as usize;
    let halves: Vec<u16> = values(n, 1)
        .iter()
        .map(|&v| (v >> 16) as u16 & 0x7bff)
        .collect();
    let floats: Vec<f32> = values(n, 2).iter().map(|&v| (v >> 9) as f32).collect();
    let ids = values(n, 3);
    for comp in [
        Compression::None,
        Compression::Rle,
        Compression::Zip,
        Compression::Piz,
        Compression::Zstd,
    ] {
        for order in [LineOrder::IncreasingY, LineOrder::DecreasingY] {
            let h = header(dw, comp, order);
            let mut out = WriteOptions::new()
                .create_writer(
                    "<memory>",
                    Cursor::new(Vec::new()),
                    std::slice::from_ref(&h),
                )
                .unwrap();
            let mut fb = FrameBuffer::new();
            fb.insert_plane("Y", &halves, dw).unwrap();
            fb.insert_plane("Z", &floats, dw).unwrap();
            fb.insert_plane("id", &ids, dw).unwrap();
            // in three calls, across chunk boundaries
            let mut part = out.part(0).unwrap();
            for lines in [7, 20, 14] {
                part.write_pixels(&fb, lines).unwrap();
            }
            let bytes = out.finish().unwrap().into_inner();

            let file = ReadOptions::new().open_bytes("<memory>", bytes).unwrap();
            let read = file.headers()[0].clone();
            assert_eq!(read.channels(), h.channels());
            assert_eq!(read.compression(), comp);
            let part = file.part(0).unwrap();
            let (mut y2, mut z2, mut id2) = (vec![0u16; n], vec![0f32; n], vec![0u32; n]);
            let mut fb = FrameBuffer::new();
            fb.insert_plane("Y", &mut y2, dw).unwrap();
            fb.insert_plane("Z", &mut z2, dw).unwrap();
            fb.insert_plane("id", &mut id2, dw).unwrap();
            part.read_pixels(&mut fb, dw.min.y, dw.max.y).unwrap();
            drop(fb);
            assert_eq!(y2, halves, "{comp:?} {order:?}");
            assert_eq!(
                z2.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
                floats.iter().map(|f| f.to_bits()).collect::<Vec<_>>()
            );
            assert_eq!(id2, ids);
        }
    }
}

#[test]
fn a_channel_without_a_slice_is_written_as_zeros() {
    let dw = window(5, 3);
    let h = header(dw, Compression::Zip, LineOrder::IncreasingY);
    let mut out = WriteOptions::new()
        .create_writer(
            "<memory>",
            Cursor::new(Vec::new()),
            std::slice::from_ref(&h),
        )
        .unwrap();
    out.part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 3)
        .unwrap();
    let bytes = out.finish().unwrap().into_inner();
    let img =
        openexr::Image::from_file(&ReadOptions::new().open_bytes("m", bytes).unwrap()).unwrap();
    for ch in &img.parts[0].channels {
        assert!(ch.pixels.as_bytes().iter().all(|&b| b == 0));
    }
}

#[test]
fn the_writer_refuses_what_the_cpp_library_refuses() {
    let dw = window(5, 3);
    let h = header(dw, Compression::None, LineOrder::IncreasingY);
    let mut out = WriteOptions::new()
        .create_writer(
            "<memory>",
            Cursor::new(Vec::new()),
            std::slice::from_ref(&h),
        )
        .unwrap();
    // a slice of another type than its channel's
    let wrong = [0f32; 15];
    let mut fb = FrameBuffer::new();
    fb.insert_plane("Y", &wrong, dw).unwrap();
    assert!(out.part(0).unwrap().write_pixels(&fb, 1).is_err());
    // more lines than the data window holds
    assert!(
        out.part(0)
            .unwrap()
            .write_pixels(&FrameBuffer::new(), 4)
            .is_err()
    );
    // a slice too small for its lines
    let short = [0u16; 4];
    let mut fb = FrameBuffer::new();
    let id = fb.add_buffer(&short[..]);
    fb.insert(
        "Y",
        openexr::Slice::new(PixelType::Half, id, 0, dw.min, 2, 10),
    )
    .unwrap();
    assert!(out.part(0).unwrap().write_pixels(&fb, 1).is_err());
    // finished before every line is written
    out.part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 2)
        .unwrap();
    assert!(out.finish().is_err());
}

#[test]
fn parts_are_written_in_order() {
    let dw = window(4, 4);
    let mut a = header(dw, Compression::Zip, LineOrder::IncreasingY);
    a.set_name("a");
    let mut b = header(dw, Compression::Rle, LineOrder::IncreasingY);
    b.set_name("b");
    let mut out = WriteOptions::new()
        .create_writer("<memory>", Cursor::new(Vec::new()), &[a, b])
        .unwrap();
    // Core takes the parts one after another
    assert!(
        out.part(1)
            .unwrap()
            .write_pixels(&FrameBuffer::new(), 4)
            .is_err()
    );
    out.part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 4)
        .unwrap();
    out.part(1)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 4)
        .unwrap();
    let bytes = out.finish().unwrap().into_inner();
    let file = MultiPartInputFile::open_bytes_for_test(bytes);
    assert_eq!(file.parts(), 2);
    assert_eq!(file.headers()[1].name(), Some(&b"b"[..]));
}

#[test]
fn a_single_part_header_keeps_its_type_and_name() {
    let dw = window(4, 4);
    let mut h = header(dw, Compression::None, LineOrder::IncreasingY);
    h.insert(
        "type",
        openexr::Attribute::String(b"scanlineimage".to_vec()),
    );
    h.set_name("single");
    let mut out = WriteOptions::new()
        .create_writer(
            "<memory>",
            Cursor::new(Vec::new()),
            std::slice::from_ref(&h),
        )
        .unwrap();
    out.part(0)
        .unwrap()
        .write_pixels(&FrameBuffer::new(), 4)
        .unwrap();
    let bytes = out.finish().unwrap().into_inner();
    let file = ReadOptions::new().open_bytes("m", bytes).unwrap();
    let read = &file.headers()[0];
    assert_eq!(read.part_type(), Some(&b"scanlineimage"[..]));
    assert_eq!(read.name(), Some(&b"single"[..]));
    // and its pixels still read: the chunk table moved with the header
    let img = openexr::Image::from_file(&file).unwrap();
    assert_eq!(img.parts.len(), 1);
}

trait OpenBytes {
    fn open_bytes_for_test(bytes: Vec<u8>) -> MultiPartInputFile;
}

impl OpenBytes for MultiPartInputFile {
    fn open_bytes_for_test(bytes: Vec<u8>) -> MultiPartInputFile {
        ReadOptions::new().open_bytes("<memory>", bytes).unwrap()
    }
}

#[test]
fn tiles_written_in_any_order_read_back_as_written() {
    use openexr::{LevelMode, RoundingMode, TileDescription};
    let dw = window(37, 21);
    let n = (dw.width() * dw.height()) as usize;
    let halves: Vec<u16> = values(n, 9)
        .iter()
        .map(|&v| (v >> 16) as u16 & 0x7bff)
        .collect();
    for order in [
        LineOrder::IncreasingY,
        LineOrder::DecreasingY,
        LineOrder::RandomY,
    ] {
        let mut h = Header::new(dw);
        h.insert_channel(Channel::new("Y", PixelType::Half));
        h.set_line_order(order);
        h.set_tiles(TileDescription {
            x_size: 8,
            y_size: 5,
            level_mode: LevelMode::One,
            rounding_mode: RoundingMode::Down,
        });
        let mut out = WriteOptions::new()
            .create_writer(
                "<memory>",
                Cursor::new(Vec::new()),
                std::slice::from_ref(&h),
            )
            .unwrap();
        let mut fb = FrameBuffer::new();
        fb.insert_plane("Y", &halves, dw).unwrap();
        let mut part = out.part(0).unwrap();
        assert!(
            part.write_pixels(&fb, 1).is_err(),
            "scanline writes of a tiled part"
        );
        let (nx, ny) = part.num_tiles((0, 0)).unwrap();
        // the middle row first, then the rest backwards
        part.write_tiles(&fb, (0, nx - 1), (ny / 2, ny / 2), (0, 0))
            .unwrap();
        for ty in (0..ny).rev().filter(|&t| t != ny / 2) {
            for tx in (0..nx).rev() {
                part.write_tile(&fb, tx, ty, (0, 0)).unwrap();
            }
        }
        assert!(
            part.write_tile(&fb, 0, 0, (0, 0)).is_err(),
            "a tile written twice"
        );
        let bytes = out.finish().unwrap().into_inner();
        let img =
            openexr::Image::from_file(&ReadOptions::new().open_bytes("m", bytes).unwrap()).unwrap();
        assert_eq!(
            img.parts[0].channels[0].pixels,
            openexr::Pixels::Half(halves.clone())
        );
    }
}
