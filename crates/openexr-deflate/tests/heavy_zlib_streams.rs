// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Heavy tier (PLAN.md §8): streams the platform's zlib makes (tests/zlib_streams.py), at
//! every level and strategy, decompress to their payloads; the Adler-32 of each payload is
//! zlib's. They reach every block type and both decode loops; the end-to-end check against
//! OpenEXR's own libdeflate is the ZIP decode against the wheel (openexr-core's
//! tests/heavy_decode.rs).

use std::path::Path;
use std::process::Command;

use openexr_deflate::{Decompressor, LibdeflateResult, adler32};

#[test]
fn zlib_streams_decompress_to_their_payloads() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let out = std::env::temp_dir().join(format!("openexr_rs_zlib_streams_{}", std::process::id()));
    let status = Command::new("uv")
        .args(["run", "--no-project", "python", "-I"])
        .arg(here.join("zlib_streams.py"))
        .arg(&out)
        .status()
        .expect("uv runs python");
    assert!(status.success());

    let manifest = std::fs::read_to_string(out.join("manifest.tsv")).expect("manifest");
    let mut d = Decompressor::new();
    let mut streams = 0;
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let (name, adler) = line.split_once('\t').expect("two columns");
        let payload = std::fs::read(out.join(format!("{name}.in"))).expect("payload");
        assert_eq!(
            adler32(1, Some(&payload)),
            adler.parse::<u32>().unwrap(),
            "{name}: Adler-32"
        );

        for entry in std::fs::read_dir(&out).expect("streams").flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some(rest) = file.strip_prefix(&format!("{name}.")) else {
                continue;
            };
            if rest == "in" {
                continue;
            }
            let stream = std::fs::read(entry.path()).expect("stream");
            let mut got = vec![0u8; payload.len()];
            let (mut in_used, mut out_used) = (0, 0);
            let r = if rest.ends_with(".zlib") {
                d.zlib_decompress_ex(&stream, &mut got, Some(&mut in_used), Some(&mut out_used))
            } else {
                d.deflate_decompress_ex(&stream, &mut got, Some(&mut in_used), Some(&mut out_used))
            };
            assert_eq!(r, LibdeflateResult::Success, "{file}");
            assert_eq!((in_used, out_used), (stream.len(), payload.len()), "{file}");
            assert!(got == payload, "{file}: bytes differ");
            // with the exact size and no actual size asked for
            let mut exact = vec![0u8; payload.len()];
            let r = if rest.ends_with(".zlib") {
                d.zlib_decompress(&stream, &mut exact, None)
            } else {
                d.deflate_decompress(&stream, &mut exact, None)
            };
            assert_eq!(r, LibdeflateResult::Success, "{file}");
            assert!(exact == payload, "{file}: bytes differ");
            streams += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&out);
    eprintln!("streams decompressed: {streams}");
    assert!(streams > 0);
}
