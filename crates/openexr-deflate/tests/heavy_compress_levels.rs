// SPDX-License-Identifier: MIT
// Copyright 2016 Eric Biggers

//! Heavy tier (PLAN.md §8): the zlib stream of every compression level, 0 to 12, byte for byte
//! as OpenEXR's own libdeflate makes it: `core_harness compress-buffer` (the source-built
//! Core harness, PLAN.md O1: `exr_compress_buffer`, which is `libdeflate_zlib_compress`). The
//! inputs reach the near-optimal compressor's paths: blocks ended by the block splitter and
//! rewound, the match cache overflowing, blocks best as literals only or static, the window
//! sliding, the passthrough sizes, and real files.
//!
//! The harness is built by `scripts/oracle-native-build.cmd` (Windows) or
//! `scripts/oracle-native-build.sh` (Linux).

use std::path::{Path, PathBuf};
use std::process::Command;

use openexr_deflate::{Compressor, zlib_compress_bound};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The harness, which the heavy tier needs built.
fn harness() -> PathBuf {
    let p = if cfg!(windows) {
        root().join("target/oracle-native/msvc-Release/core_harness.exe")
    } else {
        root().join("target/rocky9/oracle-native/gcc-Release/core_harness")
    };
    assert!(
        p.exists(),
        "{} is missing: build it with scripts/oracle-native-build",
        p.display()
    );
    p
}

/// A xorshift64 generator: the inputs only need to be varied and repeatable.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random(rng: &mut Rng, n: usize, alphabet: u64) -> Vec<u8> {
    (0..n).map(|_| rng.below(alphabet) as u8).collect()
}

fn text(rng: &mut Rng, n: usize) -> Vec<u8> {
    const WORDS: [&[u8]; 12] = [
        b"open",
        b"exr",
        b"half",
        b"pixel",
        b"chunk",
        b"line",
        b"tile",
        b"zip",
        b"the",
        b"of",
        b"deflate",
        b"scanline",
    ];
    let mut out = Vec::with_capacity(n + 16);
    while out.len() < n {
        out.extend_from_slice(WORDS[rng.below(WORDS.len() as u64) as usize]);
        out.push(if rng.below(9) == 0 { b'\n' } else { b' ' });
    }
    out.truncate(n);
    out
}

fn runs(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n + 300);
    while out.len() < n {
        let b = rng.below(256) as u8;
        let len = 1 + rng.below(300) as usize;
        out.extend(std::iter::repeat_n(b, len));
    }
    out.truncate(n);
    out
}

/// Half-float-like pixel planes: a smooth gradient with a little noise, split into the high
/// and low bytes as ZIP's predictor-less layout would be.
fn pixels(rng: &mut Rng, n: usize) -> Vec<u8> {
    let mut lo = Vec::with_capacity(n / 2);
    let mut hi = Vec::with_capacity(n / 2);
    for i in 0..n / 2 {
        let v = 0x3c00u16
            .wrapping_add((i % 1024) as u16)
            .wrapping_add(rng.below(4) as u16);
        lo.push(v as u8);
        hi.push((v >> 8) as u8);
    }
    lo.extend(hi);
    lo
}

fn inputs() -> Vec<(String, Vec<u8>)> {
    let mut rng = Rng(0x9E3779B97F4A7C15);
    let mut v: Vec<(String, Vec<u8>)> = Vec::new();
    // around each level's passthrough size (55 - 4 * level) and the minimum block length
    for n in [
        0usize, 1, 2, 6, 7, 8, 11, 12, 15, 16, 30, 54, 55, 56, 100, 511, 512, 4999, 5000, 5001,
    ] {
        v.push((format!("text-{n}"), text(&mut rng, n)));
        v.push((format!("random-{n}"), random(&mut rng, n, 256)));
    }
    for n in [70_000usize, 300_000, 305_000, 1_000_000] {
        v.push((format!("text-{n}"), text(&mut rng, n)));
        v.push((format!("random-{n}"), random(&mut rng, n, 256)));
        v.push((format!("runs-{n}"), runs(&mut rng, n)));
        v.push((format!("zeros-{n}"), vec![0u8; n]));
        v.push((format!("pixels-{n}"), pixels(&mut rng, n)));
    }
    // small alphabets: many matches at every position (the match cache overflows)
    for (alphabet, n) in [
        (2u64, 1_000_000usize),
        (3, 700_000),
        (4, 400_000),
        (16, 600_000),
    ] {
        v.push((
            format!("alphabet{alphabet}-{n}"),
            random(&mut rng, n, alphabet),
        ));
    }
    // data that changes character (the block splitter ends and rewinds blocks)
    let mut mixed = Vec::new();
    for k in 0..12 {
        match k % 4 {
            0 => mixed.extend(text(&mut rng, 40_000)),
            1 => mixed.extend(random(&mut rng, 30_000, 256)),
            2 => mixed.extend(runs(&mut rng, 50_000)),
            _ => mixed.extend(pixels(&mut rng, 60_000)),
        }
    }
    v.push(("mixed".into(), mixed));
    let mut shifts = Vec::new();
    for k in 0..40 {
        shifts.extend(random(&mut rng, 12_000, [2, 256, 5, 40][k % 4]));
    }
    v.push(("shifts".into(), shifts));
    // a period longer than the window, and one just inside it
    for period in [32_000usize, 40_000] {
        let unit = random(&mut rng, period, 256);
        let mut p = Vec::new();
        while p.len() < 400_000 {
            p.extend_from_slice(&unit);
        }
        v.push((format!("period-{period}"), p));
    }
    // real files
    let mut files = Vec::new();
    exr_files(&root().join("upstream/openexr/src/test"), &mut files);
    files.sort();
    for f in files.iter().step_by(files.len().div_ceil(16).max(1)) {
        let data = std::fs::read(f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().into_owned();
        v.push((format!("file-{name}"), data));
    }
    v
}

fn exr_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            exr_files(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exr")) {
            out.push(p);
        }
    }
}

#[test]
fn every_level_compresses_as_core_does() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_compress_levels_{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let inputs = inputs();
    let mut compared = 0;
    let mut differing = Vec::new();
    for (name, data) in &inputs {
        let src = scratch.join(format!("{name}.in"));
        std::fs::write(&src, data).unwrap();
        for level in 0..=12 {
            let dst = scratch.join(format!("{name}.{level}.zlib"));
            let out = Command::new(harness())
                .arg("compress-buffer")
                .arg(level.to_string())
                .arg(&src)
                .arg(&dst)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name} at level {level}: core_harness: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let core = std::fs::read(&dst).unwrap();
            let _ = std::fs::remove_file(&dst);

            let mut c = Compressor::new(level).expect("every level");
            let mut port = vec![0u8; zlib_compress_bound(data.len())];
            let n = c.zlib_compress(data, &mut port);
            assert!(n > 0, "{name} at level {level}: the bound is enough");
            port.truncate(n);
            if port != core {
                let at = port.iter().zip(&core).position(|(a, b)| a != b);
                differing.push(format!(
                    "{name} ({} bytes) at level {level}: port {} bytes, Core {} bytes, first difference at {at:?}",
                    data.len(),
                    port.len(),
                    core.len()
                ));
            }
            compared += 1;
        }
        let _ = std::fs::remove_file(&src);
    }
    let _ = std::fs::remove_dir_all(&scratch);
    for d in &differing {
        eprintln!("{d}");
    }
    eprintln!(
        "inputs: {}, streams compared: {compared}, differing: {}",
        inputs.len(),
        differing.len()
    );
    assert!(compared > 0);
    assert!(differing.is_empty());
}
