// SPDX-License-Identifier: BSD-3-Clause
// Copyright Contributors to the OpenEXR Project.

//! `cargo xtask guards`: the repository's mechanical rules (CLAUDE.md, PLAN.md §7).
//!
//! - every `.rs` file starts with the SPDX line and an allowed copyright line;
//! - `unsafe` appears only in the files of [`UNSAFE_ALLOWLIST`];
//! - no `#[ignore]`, `todo!`, `unimplemented!` or `dbg!`;
//! - every `Port of` citation names an upstream file that exists, with lines inside it;
//! - every ported-test marker names a test of upstream's list and sits above a `#[test]`, and
//!   the n/a tests ([`NA_TESTS`]) are upstream's and not also ported;
//! - heavy tests (`tests/heavy_*.rs`) are declared with `required-features = ["heavy"]`;
//! - `upstream-map.toml` is for the pinned commit and each entry is well formed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Files allowed to contain `unsafe` (PLAN.md D8): SIMD modules only, each added by review.
const UNSAFE_ALLOWLIST: &[&str] = &[
    // The C runtime reference for `printf` (CLAUDE.md rule 2); test-only.
    "crates/openexr-testkit/src/crt.rs",
    // The CPU reference for the SIMD emulations (owner, 2026-11-07); test-only.
    "crates/openexr-testkit/src/cpu.rs",
    // The OS error text of the default file streams (PLAN.md O7).
    "crates/openexr-core/src/os_error.rs",
    // SIMD kernels (PLAN.md O3, owner 2026-11-08: the SIMD paths).
    "crates/openexr-core/src/internal_zip_simd.rs",
    "crates/openexr-deflate/src/adler32_x86.rs",
];

/// Upstream tests that are not applicable to a Rust port, with the owner's decision (PLAN.md O8).
const NA_TESTS: &[(&str, &str)] = &[
    (
        "testInitializerCompat",
        "C initializer structs versioned by size (PLAN.md O8)",
    ),
    (
        "testBaseDebug",
        "only calls exr_print_context_info with a NULL context (PLAN.md O8)",
    ),
];

const SPDX: &str = "// SPDX-License-Identifier: BSD-3-Clause";
const COPYRIGHTS: &[&str] = &[
    "// Copyright Contributors to the OpenEXR Project.",
    // Ports of the files upstream credits to DreamWorks (PLAN.md O6).
    "// Copyright (c) DreamWorks Animation LLC and Contributors of the OpenEXR Project.",
];

/// The header of the ports of vendored libraries, by crate directory: their own license and
/// the copyright lines their files carry (CLAUDE.md, "Headers"; libdeflate's files give the
/// year each was written).
const VENDORED_HEADERS: &[(&str, &str, &[&str])] = &[
    (
        "crates/openexr-deflate/",
        "// SPDX-License-Identifier: MIT",
        &[
            "// Copyright 2016 Eric Biggers",
            "// Copyright 2020 Eric Biggers",
            "// Copyright 2022 Eric Biggers",
        ],
    ),
    (
        "crates/openexr-openjph/",
        "// SPDX-License-Identifier: BSD-2-Clause",
        &[
            "// Copyright (c) 2019, Aous Naman",
            "// Copyright (c) 2021, Aous Naman",
            "// Copyright (c) 2022, Aous Naman",
            "// Copyright (c) 2026, Osamu Watanabe",
        ],
    ),
    (
        "crates/openexr-zstd/",
        "// SPDX-License-Identifier: BSD-3-Clause",
        &["// Copyright (c) Meta Platforms, Inc. and affiliates."],
    ),
];

/// Where a citation's path is resolved, by the version after `@`.
fn citation_root(root: &Path, tag: &str) -> PathBuf {
    if tag.starts_with("Imath") {
        root.join("upstream/imath")
    } else if tag.starts_with("libdeflate") || tag.starts_with("zstd") || tag.starts_with("OpenJPH")
    {
        // libdeflate, zstd and OpenJPH as OpenEXR bundles them; their citations start at `external/`
        root.join("upstream/openexr")
    } else {
        root.join("upstream/openexr")
    }
}

pub(crate) fn run(root: &Path) -> Result<(), String> {
    let mut errors = Vec::new();
    let files = rust_files(root);
    let tests = upstream_test_names(root)?;
    let mut ported = BTreeSet::new();
    for file in &files {
        let rel = rel(root, file);
        let text = fs::read_to_string(file).map_err(|e| format!("{rel}: {e}"))?;
        check_header(&rel, &text, &mut errors);
        // xtask is tooling: it only needs the header (the workspace lints deny `unsafe` in it,
        // and its text names the patterns and citations it checks).
        if rel.starts_with("crates/") {
            check_patterns(&rel, &text, &mut errors);
            check_citations(root, &rel, &text, &tests, &mut ported, &mut errors);
        }
    }
    check_heavy_tests(root, &mut errors);
    check_upstream_map(root, &mut errors)?;
    if !errors.is_empty() {
        for e in &errors {
            eprintln!("  {e}");
        }
        return Err(format!("{} violation(s)", errors.len()));
    }
    for (name, _) in NA_TESTS {
        if !tests.contains(*name) {
            return Err(format!(
                "n/a test `{name}` is not one of upstream's Core tests"
            ));
        }
        if ported.contains(*name) {
            return Err(format!("`{name}` is both ported and listed n/a"));
        }
    }
    println!(
        "guards: {} Rust files clean; upstream tests: {} ported + {} n/a of {}",
        files.len(),
        ported.len(),
        NA_TESTS.len(),
        tests.len()
    );
    Ok(())
}

fn rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root)
        .unwrap_or(file)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Every `.rs` file of the workspace's own code (`crates/`, `xtask/`).
fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in ["crates", "xtask"] {
        walk(&root.join(dir), &mut out);
    }
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The text of a line before any `//` comment (strings are not special-cased: a forbidden
/// word in a string literal is reported too, which is the safe side).
fn code_part(line: &str) -> &str {
    line.find("//").map_or(line, |i| &line[..i])
}

fn check_header(rel: &str, text: &str, errors: &mut Vec<String>) {
    let mut lines = text.lines();
    let spdx = lines.next().unwrap_or("");
    let copyright = lines.next().unwrap_or("");
    let rel_slash = rel.replace('\\', "/");
    if let Some((_, want_spdx, want_copyrights)) = VENDORED_HEADERS
        .iter()
        .find(|(dir, _, _)| rel_slash.starts_with(dir))
    {
        if spdx != *want_spdx || !want_copyrights.contains(&copyright) {
            errors.push(format!(
                "{rel}: missing the header of its vendored library ({want_spdx}, one of {want_copyrights:?})"
            ));
        }
        return;
    }
    if spdx != SPDX || !COPYRIGHTS.contains(&copyright) {
        errors.push(format!(
            "{rel}: missing the SPDX and copyright header (CLAUDE.md)"
        ));
    }
}

fn check_patterns(rel: &str, text: &str, errors: &mut Vec<String>) {
    let allowed_unsafe = UNSAFE_ALLOWLIST.contains(&rel);
    for (n, line) in text.lines().enumerate() {
        let code = code_part(line);
        if !allowed_unsafe && contains_word(code, "unsafe") {
            errors.push(format!(
                "{rel}:{}: `unsafe` outside the allowlist (PLAN.md D8)",
                n + 1
            ));
        }
        for pat in ["#[ignore", "todo!", "unimplemented!", "dbg!"] {
            if code.contains(pat) {
                errors.push(format!(
                    "{rel}:{}: `{pat}` is forbidden (CLAUDE.md rule 4)",
                    n + 1
                ));
            }
        }
    }
}

fn contains_word(s: &str, word: &str) -> bool {
    s.match_indices(word).any(|(i, _)| {
        let before = s[..i].chars().next_back();
        let after = s[i + word.len()..].chars().next();
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        !ident(before) && !ident(after)
    })
}

/// Upstream's 73 Core tests: the `define_openexrcore_tests` list.
fn upstream_test_names(root: &Path) -> Result<BTreeSet<String>, String> {
    let path = root.join("upstream/openexr/src/test/OpenEXRCoreTest/CMakeLists.txt");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let start = text
        .find("define_openexrcore_tests(")
        .ok_or("no define_openexrcore_tests list in OpenEXRCoreTest/CMakeLists.txt")?;
    let body = &text[start + "define_openexrcore_tests(".len()..];
    let end = body
        .find(')')
        .ok_or("unterminated define_openexrcore_tests list")?;
    Ok(body[..end].split_whitespace().map(str::to_owned).collect())
}

/// Checks each `Port of `name` (path:a-b ... @ tag)` in the file. A marker in front of a
/// `#[test]` must name an upstream test and cite `src/test/OpenEXRCoreTest/`.
fn check_citations(
    root: &Path,
    rel: &str,
    text: &str,
    tests: &BTreeSet<String>,
    ported: &mut BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    let lines: Vec<&str> = text.lines().collect();
    for (n, line) in lines.iter().enumerate() {
        let Some(i) = line.find("Port of `") else {
            continue;
        };
        let at = format!("{rel}:{}", n + 1);
        // A citation may wrap onto the next comment line.
        let mut cite = line[i + "Port of `".len()..].to_string();
        if !cite.contains('@')
            && let Some(next) = lines.get(n + 1)
        {
            cite.push(' ');
            cite.push_str(next.trim_start().trim_start_matches('/').trim_start());
        }
        let Some((name, rest)) = cite.split_once('`') else {
            errors.push(format!("{at}: unterminated `Port of` name"));
            continue;
        };
        let Some(open) = rest.find('(') else {
            errors.push(format!("{at}: `Port of` without (file:lines)"));
            continue;
        };
        let inner = &rest[open + 1..];
        let Some(tag_at) = inner.find('@') else {
            errors.push(format!("{at}: `Port of` without `@ version`"));
            continue;
        };
        let tag = inner[tag_at + 1..]
            .trim()
            .trim_end_matches(['.', ')'])
            .trim();
        let loc = inner[..tag_at].trim().trim_end_matches(')').trim();
        let loc = loc.split_whitespace().next().unwrap_or("");
        let Some((path, range)) = loc.rsplit_once(':') else {
            errors.push(format!("{at}: citation `{loc}` has no line range"));
            continue;
        };
        let file = citation_root(root, tag).join(path);
        match fs::read_to_string(&file) {
            Err(_) => errors.push(format!("{at}: cited file `{path}` does not exist @ {tag}")),
            Ok(src) => {
                let count = src.lines().count();
                let bounds: Vec<Option<usize>> =
                    range.split('-').map(|x| x.parse::<usize>().ok()).collect();
                let ok = match bounds.as_slice() {
                    [Some(a)] => *a >= 1 && *a <= count,
                    [Some(a), Some(b)] => *a >= 1 && a <= b && *b <= count,
                    _ => false,
                };
                if !ok {
                    errors.push(format!(
                        "{at}: lines `{range}` are not inside `{path}` ({count} lines)"
                    ));
                }
            }
        }
        let is_test = lines[n + 1..]
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.starts_with("///"))
            .is_some_and(|l| l.starts_with("#[test]"));
        if is_test {
            if !tests.contains(name) {
                errors.push(format!(
                    "{at}: `{name}` is not one of upstream's Core tests"
                ));
            } else if !path.starts_with("src/test/OpenEXRCoreTest/") {
                errors.push(format!(
                    "{at}: a ported test cites `{path}`, not OpenEXRCoreTest"
                ));
            } else {
                ported.insert(name.to_owned());
            }
        }
    }
}

fn check_heavy_tests(root: &Path, errors: &mut Vec<String>) {
    let Ok(crates) = fs::read_dir(root.join("crates")) else {
        return;
    };
    for krate in crates.flatten() {
        let Ok(tests) = fs::read_dir(krate.path().join("tests")) else {
            continue;
        };
        let manifest = fs::read_to_string(krate.path().join("Cargo.toml")).unwrap_or_default();
        for test in tests.flatten() {
            let name = test.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".rs").filter(|s| s.starts_with("heavy_")) else {
                continue;
            };
            let declared = manifest.split("[[test]]").skip(1).any(|block| {
                block.contains(&format!("name = \"{stem}\""))
                    && block.contains("required-features = [\"heavy\"]")
            });
            if !declared {
                errors.push(format!(
                    "{}/tests/{name}: needs a [[test]] entry with required-features = [\"heavy\"]",
                    rel(root, &krate.path())
                ));
            }
        }
    }
}

/// `upstream-map.toml`: the pinned commit, and each entry well formed. The format is the one
/// `scripts/upstream_map.py` writes, read line by line.
fn check_upstream_map(root: &Path, errors: &mut Vec<String>) -> Result<(), String> {
    let text = fs::read_to_string(root.join("upstream-map.toml"))
        .map_err(|e| format!("upstream-map.toml: {e}"))?;
    let head = Command::new("git")
        .args(["-C", "upstream/openexr", "rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .map_err(|e| format!("git: {e}"))?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_owned();
    if !text.contains(&format!("commit = \"{head}\"")) {
        errors.push(format!(
            "upstream-map.toml: not for the pinned commit {head}"
        ));
    }
    for (k, block) in text.split("[[file]]").skip(1).enumerate() {
        let field = |key: &str| {
            block.lines().find_map(|l| {
                let v = l.strip_prefix(key)?.trim_start().strip_prefix('=')?.trim();
                Some(v.trim_matches('"').to_owned())
            })
        };
        let path = field("path").unwrap_or_default();
        let at = format!("upstream-map.toml entry {} (`{path}`)", k + 1);
        if path.is_empty() || !root.join("upstream/openexr").join(&path).is_file() {
            errors.push(format!("{at}: no such upstream file"));
        }
        let status = field("status").unwrap_or_default();
        match status.as_str() {
            "todo" => {}
            "n/a" => {
                if field("note").is_none() {
                    errors.push(format!("{at}: n/a needs a note"));
                }
            }
            "partial" | "done" => match field("target") {
                None => errors.push(format!("{at}: {status} needs a target")),
                Some(t) => {
                    for target in t.split(',').map(str::trim) {
                        if !root.join(target).is_file() {
                            errors.push(format!("{at}: target `{target}` does not exist"));
                        }
                    }
                }
            },
            other => errors.push(format!("{at}: unknown status `{other}`")),
        }
    }
    Ok(())
}
