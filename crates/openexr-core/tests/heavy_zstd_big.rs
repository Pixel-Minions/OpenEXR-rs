// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier: a ZSTD chunk whose zstd frame passes `ZSTD_CURRENT_MAX` (3500 MiB), so that
//! zstd corrects its indices within it (`ZSTD_overflowCorrectIfNeeded`), compressed by Core
//! (`core_harness zstd-big`, `oracle/native/core_harness.c`) and by the port the same way,
//! at levels of each strategy family, one after another on one thread (the thread's zstd
//! context reused, its indices continuing from frame to frame): the compressed bytes'
//! digests compared, level by level.
//!
//! The part is one line of two HALF channels and one FLOAT. In the first check, one frame:
//! at 470 million pixels its inner stream is 3.76 GB (each segment under `INT_MAX`),
//! corrected about 90 MB before its end, at each level; each side takes about 20 GB of
//! memory, so they run one after the other. In the second, frames of 1 GB, four at each
//! level: their indices continue from frame to frame (upstream's `ZSTDirp_continue`) and the
//! fourth (or, after a level's change of tables, an earlier one) passes the maximum.
//! `OPENEXR_RS_ZSTD_BIG` and `OPENEXR_RS_ZSTD_FRAMES` set the width and levels
//! (`<width> <level>...`); `OPENEXR_RS_HARNESS` a harness other than the one
//! `scripts/oracle-native-build` builds.
//!
//! Level 22's frames over 64 MiB, here all of them, take zstd's long distance matcher
//! (`ZSTD_resolveEnableLdm`: btopt and up with a window of 2^27).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use openexr_core::coding::BufferRef;
use openexr_core::context::{Context, DefaultWriteMode, Error};
use openexr_core::encoding::EncodePipeline;
use openexr_core::openexr_attr::{Compression, PerceptualTreatment, PixelType, Storage};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The harness, which the heavy tier needs built.
fn harness() -> PathBuf {
    if let Some(p) = std::env::var_os("OPENEXR_RS_HARNESS") {
        return PathBuf::from(p);
    }
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

/// The harness's `zstd_big_hash`.
fn zstd_big_hash(mut v: u32) -> u32 {
    v ^= v >> 16;
    v = v.wrapping_mul(0x7feb352d);
    v ^= v >> 15;
    v = v.wrapping_mul(0x846ca68b);
    v ^= v >> 16;
    v
}

/// The harness's `zstd_big_value`: channel `c`'s value at column `x`.
fn zstd_big_value(c: usize, x: u32) -> u32 {
    match c {
        0 => zstd_big_hash(x % 65521) & 0x3FFF,
        1 => (zstd_big_hash((x % 40009) + x / 1000003) >> 7) & 0xFFFF,
        _ => 0x3F800000 | (zstd_big_hash(x % 30011) & 0x007FFF00) | ((x >> 13) & 0xFF),
    }
}

/// The compressed bytes' FNV-1a 64 digest and their count, kept by [`digest_write`].
struct Digest {
    h: u64,
    n: usize,
}

fn digest_write(enc: &mut EncodePipeline, _: &mut Context, _: &[&[u8]]) -> Result<(), Error> {
    let n = enc.compressed_bytes;
    let mut h: u64 = 14695981039346656037;
    for &b in &enc.compressed()[..n] {
        h = (h ^ u64::from(b)).wrapping_mul(1099511628211);
    }
    let d = enc
        .encoding_user_data
        .as_mut()
        .unwrap()
        .downcast_mut::<Digest>()
        .unwrap();
    d.h = h;
    d.n = n;
    Ok(())
}

/// The harness's line for `level`, from the port.
fn port_zstd_big(dir: &Path, planes: &[Vec<u8>], w: u32, level: i32) -> Result<String, Error> {
    let path = dir.join("zstd-big.exr");
    let mut f = Context::start_write(
        Some(path.to_str().unwrap()),
        DefaultWriteMode::WriteFileDirectly,
        None,
    )?;
    let p = f.add_part(Some(""), Storage::SCANLINE)?;
    f.initialize_required_attr_simple(p, w as i32, 1, Compression::ZSTD)?;
    for (name, t) in [
        ("A", PixelType::HALF),
        ("B", PixelType::HALF),
        ("C", PixelType::FLOAT),
    ] {
        f.add_channel(p, Some(name), t, PerceptualTreatment::LINEAR, 1, 1)?;
    }
    f.set_zstd_compression_level(p, level)?;
    f.write_header()?;
    let ci = f.write_scanline_chunk_info(p, 0)?;
    let mut enc = f.encoding_initialize(p, &ci)?;
    for (c, ch) in enc.channels.iter_mut().enumerate() {
        let bpe = i32::from(ch.bytes_per_element);
        ch.data = Some(BufferRef {
            buffer: c,
            offset: 0,
        });
        ch.user_pixel_stride = bpe;
        ch.user_line_stride = (w as i64 * i64::from(bpe)) as i32;
    }
    f.encoding_choose_default_routines(p, &mut enc)?;
    enc.write_fn = Some(digest_write);
    enc.encoding_user_data = Some(Box::new(Digest { h: 0, n: 0 }));
    let ins: Vec<&[u8]> = planes.iter().map(|b| &b[..]).collect();
    f.encoding_run(p, &mut enc, &ins)?;
    let packed = enc.packed_bytes;
    let d = enc
        .encoding_user_data
        .take()
        .unwrap()
        .downcast::<Digest>()
        .unwrap();
    f.encoding_destroy(&mut enc)?;
    drop(f);
    let _ = std::fs::remove_file(&path);
    Ok(format!("zstd {level} {packed} {} {:016x}", d.n, d.h))
}

/// The checks run one at a time: each side takes up to 20 GB.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// The harness's lines and the port's for `spec` (`<width> <level>...`), compared.
fn compare(spec: &str) {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut args = spec.split_whitespace();
    let w: u32 = args.next().unwrap().parse().unwrap();
    let levels: Vec<i32> = args.map(|l| l.parse().unwrap()).collect();
    let dir = root().join("target/heavy-zstd-big");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Core first, then the port: each side's memory is freed before the other's
    let out = Command::new(harness())
        .arg("zstd-big")
        .arg(&dir)
        .arg(w.to_string())
        .args(levels.iter().map(|l| l.to_string()))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "core_harness zstd-big: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let core: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();

    let planes: Vec<Vec<u8>> = (0..3)
        .map(|c| {
            let bpe = if c == 2 { 4 } else { 2 };
            let mut v = Vec::with_capacity(w as usize * bpe);
            for x in 0..w {
                v.extend_from_slice(&zstd_big_value(c, x).to_le_bytes()[..bpe]);
            }
            v
        })
        .collect();
    let mut port = Vec::new();
    for &level in &levels {
        let line = port_zstd_big(&dir, &planes, w, level)
            .unwrap_or_else(|e| panic!("level {level}: {e:?}"));
        eprintln!(
            "core: {}\nport: {line}",
            core.get(port.len()).map_or("", |s| s)
        );
        port.push(line);
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(core.len(), levels.len(), "the harness's lines");
    assert_eq!(core, port);
}

#[test]
fn a_zstd_frame_past_the_index_maximum_compresses_as_core_does() {
    compare(
        &std::env::var("OPENEXR_RS_ZSTD_BIG")
            .unwrap_or_else(|_| "470000000 1 3 5 9 13 16 19 22".to_owned()),
    );
}

#[test]
fn zstd_frames_continuing_past_the_index_maximum_compress_as_core_does() {
    compare(
        &std::env::var("OPENEXR_RS_ZSTD_FRAMES").unwrap_or_else(|_| {
            "125000000 1 1 1 1 9 9 9 9 13 13 13 13 16 16 16 16 22 22 22 22".to_owned()
        }),
    );
}
