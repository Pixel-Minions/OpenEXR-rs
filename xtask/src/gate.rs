// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! `cargo xtask gate`: the checks every chunk passes (PLAN.md §8, "Test tiers").

use std::path::Path;
use std::process::Command;
use std::time::Instant;

pub(crate) fn run(root: &Path, args: &[String]) -> Result<(), String> {
    let mut full = false;
    for arg in args {
        match arg.as_str() {
            "--full" => full = true,
            other => return Err(format!("gate: unknown option `{other}`")),
        }
    }
    let start = Instant::now();
    cargo(root, &["fmt", "--all", "--check"])?;
    cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    step("guards", || crate::guards::run(root))?;
    cargo(root, &["test", "--workspace", "--quiet"])?;
    if full {
        // The heavy tier. The Rocky Linux 9 run, oracle fixtures, corpus and SDE join it when
        // they are built (PLAN.md §8); until then, say so instead of passing silently.
        cargo(
            root,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--features",
                "openexr-rs-core/heavy,openexr-rs-deflate/heavy,openexr-rs-openjph/heavy,openexr-rs-zstd/heavy",
                "--",
                "-D",
                "warnings",
            ],
        )?;
        cargo(
            root,
            &[
                "test",
                "--workspace",
                "--release",
                "--quiet",
                "--features",
                "openexr-rs-core/heavy,openexr-rs-deflate/heavy,openexr-rs-openjph/heavy,openexr-rs-zstd/heavy",
            ],
        )?;
        println!(
            "gate --full: Rocky Linux 9 is `scripts/rocky9.sh cargo xtask gate --full`; not yet \
             part of the heavy tier: oracle fixtures, corpus, SDE (PLAN.md §8)"
        );
    }
    println!(
        "gate{} passed in {:.1} s",
        if full { " --full" } else { "" },
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn step(name: &str, f: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    println!("== {name}");
    f().map_err(|e| format!("{name}: {e}"))
}

fn cargo(root: &Path, args: &[&str]) -> Result<(), String> {
    let name = format!("cargo {}", args.join(" "));
    step(&name, || {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = Command::new(cargo)
            .args(args)
            .current_dir(root)
            .status()
            .map_err(|e| format!("could not run: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("failed ({status})"))
        }
    })
}
