// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Streams of stored (uncompressed) blocks, built here as RFC 1950 and 1951 lay them out, come
//! back as the bytes they hold; and the decompressor's results for a short buffer, a short
//! output, a truncated stream and a bad checksum.

use openexr_deflate::{Decompressor, LibdeflateResult, adler32};

/// Pseudo-random bytes (a 64-bit LCG).
fn data(n: usize) -> Vec<u8> {
    let mut x: u64 = 0x5eed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (x >> 56) as u8
        })
        .collect()
}

/// A DEFLATE stream of stored blocks of at most `block` bytes holding `payload`.
fn stored(payload: &[u8], block: usize) -> Vec<u8> {
    let mut s = Vec::new();
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![&[][..]]
    } else {
        payload.chunks(block).collect()
    };
    for (i, c) in chunks.iter().enumerate() {
        // BFINAL, BTYPE 00, then padding to the byte boundary
        s.push(u8::from(i + 1 == chunks.len()));
        let len = c.len() as u16;
        s.extend_from_slice(&len.to_le_bytes());
        s.extend_from_slice(&(!len).to_le_bytes());
        s.extend_from_slice(c);
    }
    s
}

/// The zlib stream around a DEFLATE stream: CMF 0x78 (deflate, 32K window), FLG 0x01, the
/// stream, and the Adler-32 of `payload`.
fn zlib(payload: &[u8], deflate: &[u8]) -> Vec<u8> {
    let mut s = vec![0x78, 0x01];
    s.extend_from_slice(deflate);
    s.extend_from_slice(&adler32(1, Some(payload)).to_be_bytes());
    s
}

#[test]
fn stored_blocks_round_trip() {
    let mut d = Decompressor::new();
    for (n, block) in [
        (0, 1),
        (1, 1),
        (100, 7),
        (65535, 65535),
        (70000, 65535),
        (200_000, 4096),
    ] {
        let payload = data(n);
        let s = stored(&payload, block);

        let mut out = vec![0u8; n];
        let (mut in_used, mut out_used) = (0, 0);
        let r = d.deflate_decompress_ex(&s, &mut out, Some(&mut in_used), Some(&mut out_used));
        assert_eq!(r, LibdeflateResult::Success, "{n} bytes");
        assert_eq!((in_used, out_used), (s.len(), n));
        assert!(out == payload);

        let z = zlib(&payload, &s);
        let mut out = vec![0u8; n];
        let mut in_used = 0;
        assert_eq!(
            d.zlib_decompress_ex(&z, &mut out, Some(&mut in_used), None),
            LibdeflateResult::Success
        );
        assert_eq!(in_used, z.len());
        assert!(out == payload);
    }
}

#[test]
fn empty_static_block() {
    // BFINAL 1, BTYPE 01 (static Huffman), then the 7-bit end-of-block code 0000000
    let mut d = Decompressor::new();
    let mut out_used = 1;
    assert_eq!(
        d.deflate_decompress(&[0x03, 0x00], &mut [], Some(&mut out_used)),
        LibdeflateResult::Success
    );
    assert_eq!(out_used, 0);
}

#[test]
fn results_for_bad_sizes_and_data() {
    let mut d = Decompressor::new();
    let payload = data(1000);
    let s = stored(&payload, 300);

    // a buffer too small
    let mut small = vec![0u8; 999];
    assert_eq!(
        d.deflate_decompress(&s, &mut small, None),
        LibdeflateResult::InsufficientSpace
    );
    // a buffer too large, with no actual size asked for
    let mut large = vec![0u8; 1001];
    assert_eq!(
        d.deflate_decompress(&s, &mut large, None),
        LibdeflateResult::ShortOutput
    );
    let mut out_used = 0;
    assert_eq!(
        d.deflate_decompress(&s, &mut large, Some(&mut out_used)),
        LibdeflateResult::Success
    );
    assert_eq!(out_used, 1000);
    // a truncated stream
    let mut out = vec![0u8; 1000];
    assert_eq!(
        d.deflate_decompress(&s[..s.len() - 1], &mut out, None),
        LibdeflateResult::BadData
    );
    // LEN and NLEN that disagree
    let mut bad = s.clone();
    bad[3] ^= 1;
    assert_eq!(
        d.deflate_decompress(&bad, &mut out, None),
        LibdeflateResult::BadData
    );
    // a bad checksum, and a bad zlib header
    let mut z = zlib(&payload, &s);
    let last = z.len() - 1;
    z[last] ^= 1;
    assert_eq!(
        d.zlib_decompress(&z, &mut out, None),
        LibdeflateResult::BadData
    );
    let mut z = zlib(&payload, &s);
    z[1] = 0x02;
    assert_eq!(
        d.zlib_decompress(&z, &mut out, None),
        LibdeflateResult::BadData
    );
    // too short for a zlib stream
    assert_eq!(
        d.zlib_decompress(&[0x78, 0x01, 0, 0, 0], &mut out, None),
        LibdeflateResult::BadData
    );
}
