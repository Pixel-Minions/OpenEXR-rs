// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! The compressor's zlib streams decompress to their input, within the bound, at every level
//! ported so far, for inputs that reach each of its paths: empty and short inputs passed
//! through, the three block types, matches across the window slide (over 32 KiB), and blocks
//! ended by the split heuristic and by the soft maximum length (over 300 000 bytes). That the
//! streams are the ones libdeflate writes is checked against the wheel's ZIP files (heavy
//! tier, `crates/openexr-core/tests/heavy_decode.rs`).

use openexr_deflate::{Compressor, Decompressor, LibdeflateResult, zlib_compress_bound};

/// Pseudo-random bytes (a 64-bit LCG), each below `range`.
fn noise(n: usize, seed: u64, range: u32) -> Vec<u8> {
    let mut x: u64 = seed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((x >> 32) as u32 % range) as u8
        })
        .collect()
}

/// Inputs of many shapes.
fn inputs() -> Vec<(String, Vec<u8>)> {
    let mut v = vec![
        ("empty".to_string(), Vec::new()),
        ("one byte".to_string(), vec![42]),
        ("short".to_string(), b"OpenEXR".to_vec()),
        ("40 bytes".to_string(), noise(40, 1, 256)),
        ("zeros".to_string(), vec![0; 100_000]),
        ("random".to_string(), noise(70_000, 2, 256)),
        ("few symbols".to_string(), noise(50_000, 3, 4)),
        ("text-like".to_string(), noise(20_000, 4, 26)),
    ];
    // repeated phrases with noise between: matches at many offsets
    let phrase = noise(300, 5, 256);
    let mut rep = Vec::new();
    for i in 0..2000 {
        rep.extend_from_slice(&phrase[i % 7..]);
        rep.extend_from_slice(&noise(i % 13, i as u64, 256));
    }
    v.push(("repeats".to_string(), rep));
    // data whose statistics change, so the split heuristic ends blocks
    let mut shifting = noise(40_000, 6, 8);
    shifting.extend(noise(40_000, 7, 256));
    shifting.extend(vec![7; 30_000]);
    shifting.extend(noise(40_000, 8, 3));
    v.push(("shifting".to_string(), shifting));
    // over the soft maximum block length
    let mut long = Vec::new();
    for i in 0..700_000usize {
        long.push(((i / 3) % 251) as u8 ^ ((i >> 11) as u8 & 3));
    }
    v.push(("long".to_string(), long));
    v
}

#[test]
fn zlib_streams_decompress_to_their_input() {
    let mut checked = 0;
    for level in 0..=12 {
        let Some(mut c) = Compressor::new(level) else {
            continue;
        };
        for (name, input) in inputs() {
            let mut out = vec![0u8; zlib_compress_bound(input.len())];
            let n = c.zlib_compress(&input, &mut out);
            assert!(n > 0, "level {level}, {name}: no output");
            assert!(n <= out.len(), "level {level}, {name}: past the bound");

            let mut back = vec![0u8; input.len()];
            let mut d = Decompressor::new();
            let (mut used, mut got) = (0usize, 0usize);
            let res = d.zlib_decompress_ex(&out[..n], &mut back, Some(&mut used), Some(&mut got));
            assert_eq!(res, LibdeflateResult::Success, "level {level}, {name}");
            assert_eq!((used, got), (n, input.len()), "level {level}, {name}");
            assert!(back == input, "level {level}, {name}: bytes differ");
            checked += 1;
        }
    }
    assert!(checked > 0);
}

/// An output buffer one byte too small for the stream gives no output.
#[test]
fn short_buffers_give_nothing() {
    for level in 0..=12 {
        let Some(mut c) = Compressor::new(level) else {
            continue;
        };
        for (name, input) in inputs() {
            let mut out = vec![0u8; zlib_compress_bound(input.len())];
            let n = c.zlib_compress(&input, &mut out);
            let mut short = vec![0u8; n - 1];
            assert_eq!(
                c.zlib_compress(&input, &mut short),
                0,
                "level {level}, {name}"
            );
        }
    }
}
