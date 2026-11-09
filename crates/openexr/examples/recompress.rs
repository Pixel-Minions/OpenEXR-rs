// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Rewrites an OpenEXR file with another compression: every part read whole, its header's
//! compression changed, written again on every core.
//!
//!   cargo run --release -p openexr-rs --example recompress -- <in.exr> <out.exr> <compression>
//!
//! <compression> is one of none, rle, zips, zip, piz, pxr24, b44, b44a, dwaa, dwab,
//! htj2k256, htj2k32, lj2k, zstd.

use openexr::{Compression, Image, ReadOptions, Threads, WriteOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: recompress <in.exr> <out.exr> <compression>".into());
    }
    let names = [
        "none", "rle", "zips", "zip", "piz", "pxr24", "b44", "b44a", "dwaa", "dwab", "htj2k256",
        "htj2k32", "lj2k", "zstd",
    ];
    let c = names
        .iter()
        .position(|n| n.eq_ignore_ascii_case(&args[3]))
        .ok_or("unknown compression")?;
    let compression = Compression::from_value(c as u8);

    let file = ReadOptions::new().threads(Threads::Auto).open(&args[1])?;
    let mut image = Image::from_file(&file)?;
    for part in &mut image.parts {
        part.header.set_compression(compression);
    }
    let out = std::fs::File::create(&args[2])?;
    image.write_to(&args[2], out, &WriteOptions::new().threads(Threads::Auto))?;
    println!("{} -> {} ({compression:?})", args[1], args[2]);
    Ok(())
}
