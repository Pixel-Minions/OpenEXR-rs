// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The port's side of `core_harness bench` (oracle/native/core_harness.c, cmd_bench): the same
//! work through the port's API, timed the same way. A measurement, not a check.
//!   cargo run --release -p openexr-rs-core --example bench -- <in> <reps> [<compression> [--zip N] [--zstd N] [--dwa F]]

use std::time::Instant;

use openexr_core::chunk_info::ChunkInfo;
use openexr_core::coding::BufferRef;
use openexr_core::context::{Context, DefaultWriteMode, Error};
use openexr_core::decoding::DecodePipeline;
use openexr_core::encoding::EncodePipeline;
use openexr_core::openexr_attr::{Compression, LineOrder, PixelType, Storage, TileLevelMode};

fn sampled(a: i64, b: i64, s: i64) -> i64 {
    let fd = |x: i64| if x >= 0 { x / s } else { -((-x + s - 1) / s) };
    fd(b - 1) - fd(a - 1)
}

fn write_nothing(_: &mut EncodePipeline, _: &mut Context, _: &[&[u8]]) -> Result<(), Error> {
    Ok(())
}

/// Every chunk of part 0, read side or (with `out`) write side, in recompress's order.
fn chunks(src: &Context, mut out: Option<&mut Context>) -> Result<Vec<ChunkInfo>, Error> {
    let mut v = Vec::new();
    let st = src.get_storage(0)?;
    if st == Storage::SCANLINE {
        let dw = src.get_data_window(0)?;
        let lpc = match &out {
            Some(o) => o.get_scanlines_per_chunk(0)?,
            None => src.get_scanlines_per_chunk(0)?,
        };
        let mut y = dw.min.y;
        while y <= dw.max.y {
            v.push(match &mut out {
                Some(o) => o.write_scanline_chunk_info(0, y)?,
                None => src.read_scanline_chunk_info(0, y)?,
            });
            y += lpc;
        }
        return Ok(v);
    }
    assert!(st == Storage::TILED, "deep parts are not supported");
    let (nlx, nly) = src.get_tile_levels(0)?;
    let rip = src.get_tile_descriptor(0)?.2 == TileLevelMode::RIPMAP_LEVELS;
    for ly in 0..nly {
        for lx in 0..nlx {
            if !rip && lx != ly {
                continue;
            }
            let (cx, cy) = src.get_tile_counts(0, lx, ly)?;
            for ty in 0..cy {
                for tx in 0..cx {
                    v.push(match &mut out {
                        Some(o) => o.write_tile_chunk_info(0, tx, ty, lx, ly)?,
                        None => src.read_tile_chunk_info(0, tx, ty, lx, ly)?,
                    });
                }
            }
        }
    }
    Ok(v)
}

fn decode(src: &Context, ci: &ChunkInfo, buf: &mut [u8]) -> Result<(), Error> {
    let mut dec = src.decoding_initialize(0, ci)?;
    set_planes_dec(&mut dec);
    src.decoding_choose_default_routines(0, &mut dec)?;
    src.decoding_run(0, &mut dec, &mut [buf])?;
    src.decoding_destroy(&mut dec)
}

fn set_planes_dec(dec: &mut DecodePipeline<'_>) {
    let mut off = 0usize;
    for ch in dec.channels.iter_mut() {
        let bpe = i32::from(ch.bytes_per_element);
        ch.data = Some(BufferRef {
            buffer: 0,
            offset: off,
        });
        ch.user_pixel_stride = bpe;
        ch.user_line_stride = ch.width * bpe;
        off += (ch.width.max(0) * ch.height.max(0) * bpe) as usize;
    }
}

fn plane_sizes(chs: &[openexr_core::coding::CodingChannelInfo]) -> Vec<usize> {
    chs.iter()
        .map(|c| (c.width.max(0) * c.height.max(0) * i32::from(c.bytes_per_element)) as usize)
        .collect()
}

fn main() -> Result<(), Error> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reps: usize = args[1].parse().unwrap();
    let src = Context::start_read(Some(&args[0]), None)?;
    let rd = chunks(&src, None)?;
    let maxsz = rd
        .iter()
        .map(|c| c.unpacked_size as usize)
        .max()
        .unwrap_or(0);
    let mut scratch = vec![0u8; maxsz + 1];
    let mut best = f64::MAX;
    let dreps = if std::env::var_os("PB_ENC_ONLY").is_some() {
        1
    } else {
        reps
    };
    for _ in 0..dreps {
        let t0 = Instant::now();
        for ci in &rd {
            decode(&src, ci, &mut scratch)?;
        }
        best = best.min(t0.elapsed().as_secs_f64());
    }
    println!("decode {} {best:.9}", rd.len());

    if args.len() < 3 {
        return Ok(());
    }
    let comp = Compression(args[2].parse().unwrap());
    let st = src.get_storage(0)?;
    let tmp = format!("{}.bench-out", args[0]);
    let mut out = Context::start_write(Some(&tmp), DefaultWriteMode::WriteFileDirectly, None)?;
    let idx = out.add_part(None, st)?;
    out.copy_unset_attributes(idx, &src, 0)?;
    out.set_compression(idx, comp)?;
    out.set_lineorder(idx, LineOrder::RANDOM_Y)?;
    let mut i = 3;
    while i + 1 < args.len() {
        match args[i].as_str() {
            "--zip" => out.set_zip_compression_level(idx, args[i + 1].parse().unwrap())?,
            "--zstd" => out.set_zstd_compression_level(idx, args[i + 1].parse().unwrap())?,
            "--dwa" => out.set_dwa_compression_level(idx, args[i + 1].parse().unwrap())?,
            o => panic!("unknown option {o}"),
        }
        i += 2;
    }
    out.write_header()?;
    let wr = chunks(&src, Some(&mut out))?;

    // the encoders' inputs, filled beforehand
    let mut inputs: Vec<Vec<u8>> = Vec::with_capacity(wr.len());
    if st == Storage::SCANLINE {
        let dw = src.get_data_window(0)?;
        let chans: Vec<(i64, i64, i64)> = src
            .get_channels(0)?
            .entries()
            .iter()
            .map(|e| {
                let w = sampled(
                    i64::from(dw.min.x),
                    i64::from(dw.max.x) + 1,
                    i64::from(e.x_sampling),
                );
                let bpe = if e.pixel_type == PixelType::HALF {
                    2
                } else {
                    4
                };
                (w, bpe, i64::from(e.y_sampling))
            })
            .collect();
        let mut whole: Vec<Vec<u8>> = chans
            .iter()
            .map(|&(w, bpe, ys)| {
                vec![
                    0u8;
                    (w * bpe * sampled(i64::from(dw.min.y), i64::from(dw.max.y) + 1, ys)) as usize
                        + 1
                ]
            })
            .collect();
        for ci in &rd {
            let mut dec = src.decoding_initialize(0, ci)?;
            for (c, ch) in dec.channels.iter_mut().enumerate() {
                let (w, bpe, ys) = chans[c];
                let row = sampled(i64::from(dw.min.y), i64::from(ci.start_y), ys);
                ch.data = Some(BufferRef {
                    buffer: c,
                    offset: (row * w * bpe) as usize,
                });
                ch.user_pixel_stride = i32::from(ch.bytes_per_element);
                ch.user_line_stride = (w * bpe) as i32;
            }
            src.decoding_choose_default_routines(0, &mut dec)?;
            let mut outs: Vec<&mut [u8]> = whole.iter_mut().map(|b| &mut b[..]).collect();
            src.decoding_run(0, &mut dec, &mut outs)?;
            src.decoding_destroy(&mut dec)?;
        }
        for ci in &wr {
            let mut enc = out.encoding_initialize(0, ci)?;
            let sizes = plane_sizes(&enc.channels);
            let mut buf = Vec::with_capacity(sizes.iter().sum::<usize>() + 1);
            for (c, n) in sizes.iter().enumerate() {
                let (w, bpe, ys) = chans[c];
                let row = sampled(i64::from(dw.min.y), i64::from(ci.start_y), ys);
                let at = (row * w * bpe) as usize;
                buf.extend_from_slice(&whole[c][at..at + n]);
            }
            buf.push(0);
            inputs.push(buf);
            out.encoding_destroy(&mut enc)?;
        }
    } else {
        for (k, ci) in wr.iter().enumerate() {
            decode(&src, &rd[k], &mut scratch)?;
            let dec = src.decoding_initialize(0, &rd[k])?;
            let mut enc = out.encoding_initialize(0, ci)?;
            let esz = plane_sizes(&enc.channels);
            let mut buf = vec![0u8; esz.iter().sum::<usize>() + 1];
            let (mut doff, mut eoff) = (0usize, 0usize);
            for (d, e) in dec.channels.iter().zip(&enc.channels) {
                let bpe = d.bytes_per_element as usize;
                let rows = d.height.min(e.height).max(0) as usize;
                let cols = d.width.min(e.width).max(0) as usize;
                for y in 0..rows {
                    let s = doff + y * d.width as usize * bpe;
                    let t = eoff + y * e.width as usize * bpe;
                    buf[t..t + cols * bpe].copy_from_slice(&scratch[s..s + cols * bpe]);
                }
                doff += (d.width.max(0) * d.height.max(0)) as usize * bpe;
                eoff += (e.width.max(0) * e.height.max(0)) as usize * bpe;
            }
            inputs.push(buf);
            out.encoding_destroy(&mut enc)?;
        }
    }

    let mut best = f64::MAX;
    for _ in 0..reps {
        let t0 = Instant::now();
        for (ci, input) in wr.iter().zip(&inputs) {
            let mut enc = out.encoding_initialize(0, ci)?;
            let mut off = 0usize;
            for ch in enc.channels.iter_mut() {
                let bpe = i32::from(ch.bytes_per_element);
                ch.data = Some(BufferRef {
                    buffer: 0,
                    offset: off,
                });
                ch.user_pixel_stride = bpe;
                ch.user_line_stride = ch.width * bpe;
                off += (ch.width.max(0) * ch.height.max(0) * bpe) as usize;
            }
            out.encoding_choose_default_routines(0, &mut enc)?;
            enc.write_fn = Some(write_nothing);
            out.encoding_run(0, &mut enc, &[&input[..]])?;
            out.encoding_destroy(&mut enc)?;
        }
        best = best.min(t0.elapsed().as_secs_f64());
    }
    println!("encode {} {best:.9}", wr.len());
    drop(out);
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}
