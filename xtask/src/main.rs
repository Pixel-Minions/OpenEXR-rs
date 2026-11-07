// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! Repository automation (PLAN.md §7, §8). Run `cargo xtask help`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod gate;
mod guards;

const USAGE: &str = "\
cargo xtask <command>

  gate [--full]   fmt, clippy, guards and tests, stopping at the first failure (the fast tier,
                  PLAN.md §8); --full adds the heavy tier: the `heavy` feature's tests
  guards          headers, the `unsafe` allowlist, forbidden patterns, ported-test markers
                  and upstream-map.toml
  help            this text
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    let result = match args.first().map(String::as_str) {
        Some("gate") => gate::run(&root, &args[1..]),
        Some("guards") => guards::run(&root),
        Some("help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::FAILURE
        }
    }
}

/// The repository root: the parent of this crate's directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}
