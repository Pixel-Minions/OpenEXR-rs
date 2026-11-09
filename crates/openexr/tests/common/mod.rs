// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! The heavy tests' image set and oracle calls (PLAN.md §8, §16).

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub(crate) const COMPRESSIONS: [&str; 14] = [
    "NO_COMPRESSION",
    "RLE_COMPRESSION",
    "ZIPS_COMPRESSION",
    "ZIP_COMPRESSION",
    "PIZ_COMPRESSION",
    "PXR24_COMPRESSION",
    "B44_COMPRESSION",
    "B44A_COMPRESSION",
    "DWAA_COMPRESSION",
    "DWAB_COMPRESSION",
    "HTJ2K256_COMPRESSION",
    "HTJ2K32_COMPRESSION",
    "LJ2K_COMPRESSION",
    "ZSTD_COMPRESSION",
];

pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub(crate) fn oracle(args: &[&std::ffi::OsStr]) {
    let root = root();
    let status = Command::new("uv")
        .args(["run", "--project"])
        .arg(root.join("oracle"))
        .args(["python", "-I"])
        .args(args)
        .status()
        .expect("uv runs the oracle");
    assert!(status.success(), "the oracle failed: {args:?}");
}

/// Every `.exr` under `dir`, recursively, sorted.
pub(crate) fn exr_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("a directory")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            exr_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "exr") {
            out.push(p);
        }
    }
}

pub(crate) fn write_list(path: &Path, images: &[PathBuf]) {
    let names: Vec<String> = images
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    std::fs::write(path, names.join("\n")).unwrap();
}

pub(crate) fn images(scratch: &Path) -> Vec<PathBuf> {
    let mut images = Vec::new();
    let dir = root().join("upstream/openexr/src/test/OpenEXRTest");
    images.extend(
        std::fs::read_dir(&dir)
            .expect("test images")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "exr")),
    );
    let written = scratch.join("written");
    let script = root().join("oracle/write_images.py");
    let mut args = vec![script.as_os_str(), written.as_os_str()];
    args.extend(COMPRESSIONS.iter().map(std::ffi::OsStr::new));
    oracle(&args);
    exr_files(&written, &mut images);
    images.sort();
    let corpus = root().join("corpus/openexr-images");
    assert!(
        corpus.is_dir(),
        "{} is missing: fetch it with scripts/fetch-corpus.sh",
        corpus.display()
    );
    exr_files(&corpus, &mut images);

    let damaged = scratch.join("damaged");
    std::fs::create_dir_all(&damaged).unwrap();
    // (the list in a file: a command line of every path is too long for Windows)
    let list = scratch.join("damage.txt");
    write_list(&list, &images);
    let script = "import sys\nsys.path.insert(0, sys.argv[1])\nimport native_accept_messages as m\nfor p in open(sys.argv[3], encoding='utf-8').read().split(chr(10)):\n    m.damaged(p, sys.argv[2])\n";
    let oracle_dir = root().join("oracle");
    oracle(&[
        "-c".as_ref(),
        script.as_ref(),
        oracle_dir.as_os_str(),
        damaged.as_os_str(),
        list.as_os_str(),
    ]);
    exr_files(&damaged, &mut images);
    images
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// A channel as the wheel read it (`oracle/dump_pixels.py`).
pub(crate) struct WheelChannel {
    pub(crate) part: usize,
    pub(crate) name: Vec<u8>,
    pub(crate) dtype: String,
    pub(crate) height: usize,
    pub(crate) width: usize,
    pub(crate) x_sampling: i32,
    pub(crate) y_sampling: i32,
    pub(crate) data: Vec<u8>,
    /// A deep channel's sample counts (`uint32`, row by row).
    pub(crate) counts: Option<Vec<u8>>,
}

/// The channels the wheel read from an image, `dump_pixels.py`'s output in `dir`; or its
/// refusal.
pub(crate) fn wheel_pixels(dir: &Path) -> Result<Vec<WheelChannel>, String> {
    let manifest = std::fs::read_to_string(dir.join("manifest.tsv")).expect("manifest");
    let mut chans = Vec::new();
    for line in manifest.lines().filter(|l| !l.is_empty()) {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols[0] == "error" {
            return Err(cols.get(1).unwrap_or(&"").to_string());
        }
        chans.push(WheelChannel {
            part: cols[0].parse().expect("part"),
            name: unhex(cols[2]),
            dtype: cols[3].to_owned(),
            height: cols[4].parse().expect("height"),
            width: cols[5].parse().expect("width"),
            x_sampling: cols[6].parse().expect("x sampling"),
            y_sampling: cols[7].parse().expect("y sampling"),
            data: std::fs::read(dir.join(cols[8])).expect("channel file"),
            counts: cols[3].starts_with("deep-").then(|| {
                let counts = cols[8].replace(".bin", ".counts");
                std::fs::read(dir.join(counts)).expect("counts file")
            }),
        });
    }
    Ok(chans)
}

/// The compressions a deep part may have, with the wheel's names for them.
pub(crate) const DEEP: [&str; 4] = [
    "NO_COMPRESSION",
    "RLE_COMPRESSION",
    "ZIPS_COMPRESSION",
    "ZSTD_COMPRESSION",
];

/// Deep files: the wheel's (`oracle/write_deep.py`: scanlines and tiles, each compression a
/// deep part may have, 1, 3 and 10 channels) and those of upstream's test tree.
pub(crate) fn deep_images(scratch: &Path) -> Vec<PathBuf> {
    let generated = scratch.join("generated");
    std::fs::create_dir_all(&generated).unwrap();
    let mut seed = 100;
    let script = root().join("oracle/write_deep.py");
    for comp in DEEP {
        for kind in ["scan", "tile"] {
            for nchan in [1, 3, 10] {
                seed += 1;
                let image = generated.join(format!("{kind}_{nchan}_{comp}.exr"));
                let (n, s) = (nchan.to_string(), seed.to_string());
                oracle(&[
                    script.as_os_str(),
                    image.as_os_str(),
                    kind.as_ref(),
                    n.as_ref(),
                    comp.as_ref(),
                    s.as_ref(),
                ]);
            }
        }
    }
    let mut all = Vec::new();
    exr_files(&generated, &mut all);
    exr_files(
        &root().join("upstream/openexr/src/test/bin/test_images"),
        &mut all,
    );
    all
}
