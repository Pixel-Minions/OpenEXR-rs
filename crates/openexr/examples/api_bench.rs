// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Timing for comparing the API's speed with the C++ library's (`scripts/bench_api.py`):
//! a whole file read with `Image::read`, then written with `Image::write_to` into memory,
//! `reps` times each on `threads` threads; the best times on stdout, in milliseconds.
//!
//!   cargo run --release -p openexr-rs --example api_bench -- <file> <reps> <threads>

use std::io::Cursor;
use std::time::Instant;

use openexr::{Image, ReadOptions, Threads, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: api_bench <file> <reps> <threads>".into());
    }
    let reps: usize = args[2].parse()?;
    let n: usize = args[3].parse()?;
    let threads = if n <= 1 {
        Threads::Serial
    } else {
        Threads::Count(n)
    };
    // the file in memory first: the reads time decoding, not the disk
    let bytes: std::sync::Arc<[u8]> = std::fs::read(&args[1])?.into();
    let options = ReadOptions::new().threads(threads);
    let mut read = f64::MAX;
    let mut image = None;
    for _ in 0..reps {
        let t = Instant::now();
        let file = options.open_bytes(&args[1], bytes.clone())?;
        let img = Image::from_file(&file)?;
        read = read.min(t.elapsed().as_secs_f64());
        image = Some(img);
    }
    let image = image.ok_or("no repetitions")?;
    let woptions = WriteOptions::new().threads(threads);
    let mut write = f64::MAX;
    for _ in 0..reps {
        let t = Instant::now();
        let out = image.write_to(
            "<memory>",
            Cursor::new(Vec::with_capacity(bytes.len())),
            &woptions,
        )?;
        write = write.min(t.elapsed().as_secs_f64());
        std::hint::black_box(out);
    }
    println!("read {:.3} write {:.3}", read * 1e3, write * 1e3);
    Ok(())
}
