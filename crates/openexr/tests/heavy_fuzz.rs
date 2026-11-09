// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Heavy tier: the API's mutation fuzzer (`examples/fuzz.rs`), a fixed number of cases from a
//! fixed seed, on the decode set and the deep files: damaged files read every way the API
//! reads, undamaged ones read into random frame buffer layouts, random files written from
//! random layouts (those written whole read back, as written for a lossless compression).
//! A read or write may fail; none may panic, abort or take more than a minute. Longer runs:
//! `cargo run --release -p openexr-rs --example fuzz`.

mod common;

#[path = "../examples/fuzz.rs"]
#[allow(dead_code)]
mod fuzz;

use common::{deep_images, images};

#[test]
fn damaged_files_and_random_layouts_never_panic() {
    let scratch =
        std::env::temp_dir().join(format!("openexr_rs_heavy_fuzz_{}", std::process::id()));
    let mut files = deep_images(&scratch);
    files.extend(images(&scratch));
    let seeds: Vec<Vec<u8>> = files
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .filter(|b| b.len() < (4 << 20))
        .collect();
    assert!(seeds.len() > 100, "too few seed files");
    let out = scratch.join("fuzz");
    let report = fuzz::run(
        &seeds,
        30_000,
        20261109,
        &out,
        std::time::Duration::from_secs(60),
    )
    .expect("the fuzzer's scratch");
    eprintln!(
        "{} cases: {} panics, {} slow; {} random writes ended a file (cases in {})",
        report.cases,
        report.panics.len(),
        report.slow,
        report.written,
        out.display()
    );
    assert!(report.panics.is_empty(), "panics: {:#?}", report.panics);
    assert_eq!(
        report.slow,
        0,
        "cases slower than a minute (kept in {})",
        out.display()
    );
    assert!(report.written > 100, "too few random writes ended a file");
    let _ = std::fs::remove_dir_all(&scratch);
}
