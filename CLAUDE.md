# OpenEXR-rs: rules for agents

An exact Rust port of OpenEXR **3.5.3**, beginning with OpenEXRCore, built by agents and
accepted only on byte-by-byte parity with the official `openexr==3.5.3` wheel. `PLAN.md` is the
source of truth: read §3 (definition of done), §7 (how agents work), §8 (verification) and your
card before writing code. The rules are OpenColorIO-rs's (`D:\Projects\OpenColorIO-rs\CLAUDE.md`),
translated from C++ to C; where this file is silent, that one applies.

**Current stage (owner, 2026-11-07):** porting OpenEXRCore, phase by phase (`PLAN.md` §10).
Everything runs locally: no CI, no profiling, and never push (remote:
github.com/Pixel-Minions/OpenEXR-rs) unless the owner asks. The owner's open decisions are in
`PLAN.md` §14; don't decide them yourself.

**Test tiers (owner, 2026-11-07, `PLAN.md` §8).** Every chunk passes the fast tier,
`cargo xtask gate`. Heavy checks (the Rocky Linux run, oracle fixtures and corpus, exhaustive
sweeps, SDE) are deferred: write them with the chunk, behind the `heavy` feature
(`#[cfg(feature = "heavy")]` in the crate, or `tests/heavy_*.rs` with
`required-features = ["heavy"]`), and leave the chunk `partial` in `upstream-map.toml` until
`cargo xtask gate --full` has run them. Deferring a test is not skipping it.

## Identity

Every commit is authored as `Jose Enriquez <contact@pixelminions.com>`. The repository's own git
config already says so; the global config is a work address and must never be used here. Check
`git config user.email` before your first commit, and never change git config. Commit messages
end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## The rules

1. **Port, don't reinvent.**
   - Read the upstream code in `upstream/openexr` (submodule pinned at `v3.5.3`) and translate it.
   - Every ported function's doc comment cites its source:
     `` Port of `exr_decoding_run` (src/lib/OpenEXRCore/decoding.c:563-711 @ v3.5.3). ``
     Vendored code cites its own version: `@ libdeflate v1.26`, `@ zstd v1.5.7`,
     `@ OpenJPH 0.32.0`, `@ Imath v3.2.3`.
   - Don't guess behavior, simplify it, or "fix" it. Upstream bugs that change outputs are ported
     too, and listed in `docs/improvements.md` in the same chunk.
   - Where upstream reads or writes memory it doesn't own, return an error instead and list it
     as a `U-` entry; where both wheels resolve undefined behavior the same predictable way, match
     them and list it as an `I-` entry. If neither fits, stop and report it.
   - Translating a vendored library? Add its notice to `THIRD-PARTY-NOTICES.md` in the same chunk.
2. **Expected values come only from the oracle, upstream's tests, or the platform itself.**
   - The oracle is the real OpenEXR 3.5.3 (`oracle/`; a source-built Core harness only if the
     owner approves PLAN.md O1, and only where it matches the wheel).
   - Upstream tests are copied verbatim, with a citation.
   - The platform C runtime is the reference for `printf` and libm behavior, and the CPU for
     instruction semantics.
   - Never use the port's own output as an expected value. Never type a number into a test that
     didn't come from one of those places.
3. **Comparisons are exact.** Pixels compare bitwise, files and chunks byte for byte, text byte for
   byte. The only tolerances allowed are upstream's own, in ported upstream tests.
4. **Never weaken a check.** No `#[ignore]`, `todo!()` or `unimplemented!()`, no loosened
   comparison. Never edit `fixtures/`, `oracle/uv.lock` or waivers to get green. If you can't
   explain a mismatch, stop: write a minimal reproduction (an oracle probe plus a failing test),
   leave it failing, and report it.
5. **Match upstream's arithmetic exactly.** See "Bit-exact porting" below.
6. **Stay in scope.** Change only the files your card names.
7. **Work in small, mergeable chunks.** One upstream file or module with its tests, under ~600
   lines of non-test code, green on its own. Bookkeeping (`upstream-map.toml`) travels with the
   port. Oracle changes stand alone, before the chunk that first uses them. Commit on your own
   branch; never rewrite earlier commits.

## Bit-exact porting (C)

- **Integer semantics.**
  - Usual arithmetic conversions and integer promotions: `uint16_t * uint16_t` multiplies as
    `int`; `uint8_t` shifts promote to `int`; a `size_t` mixed with `int` converts the `int`.
    Reproduce the widths C computes in, not the widths of the variables.
  - Unsigned arithmetic wraps: use `wrapping_*` where C can wrap.
  - Signed overflow, shifts by the width or more, and out-of-range float-to-int conversions are
    undefined in C. x86 `cvttss2si` gives `0x80000000`; Rust `as` saturates and turns NaN into 0.
    Where upstream can reach such a case, do what the wheels' machine code does (an `I-` entry)
    or refuse it (`U-`).
- **Floats.** x86-64 C evaluates `float` in single precision (SSE, `FLT_EVAL_METHOD` 0). Keep
  every `float`/`double` boundary and the operation order exactly as in C. Neither wheel is
  built with FMA enabled for Core's own files; Rust never fuses either. Use `mul_add` only where
  upstream calls an FMA intrinsic or a file is compiled with FMA enabled (check the wheel's
  machine code; OpenJPH builds its AVX2/AVX-512 files with their own flags).
- **min, max, clamp, rounding.** Port the C expression as written (`a < b ? a : b` and so on);
  never substitute `f32::min`/`max`/`clamp`/`round`, which treat NaN and ties differently.
- **Half.** Core converts through Imath 3.2.3's `imath_half_to_float`/`imath_float_to_half`
  (`internal_coding.h`, no lookup table, no F16C in the wheel's build of that header). Port that,
  not the `half` crate's conversion, unless a test over all 2^16 and 2^32 inputs proves them
  identical.
- **Formatting.** Error and debug text uses `printf` (`%d`, `%s`, `%g`, `%u`, `%08X`, `PRIu64`,
  ...). Format exactly as the platform's C runtime does; `%g` in particular is not Rust's `{}`.
  `%p` prints addresses, which can't match: report such cases.
- **SIMD.** Scalar first. Each SIMD path upstream has (DWA SSE2/AVX/F16C, the ZIP predictor,
  zstd's transforms, OpenJPH's variants) is its own kernel, chosen exactly as upstream chooses it.
  Upstream's `check_for_x86_simd` never reports AVX or F16C under MSVC (the inline-assembly probe
  fails, so `xgetbv` reads as 0): on Windows the port must take the same non-AVX paths.
  `unsafe` only in SIMD modules, through the allowlist `xtask guards` will enforce.
- **Platform differences.** Where the Windows and Linux wheels differ (DWA today:
  `docs/baseline.md`), the port must differ the same way. Check both platforms.

## Layout and conventions

- **Where code goes.** Files mirror upstream: `src/lib/OpenEXRCore/parse_header.c` becomes
  `crates/openexr-core/src/parse_header.rs` (layout pending PLAN.md O4). `upstream-map.toml`
  records each upstream file's target and status (`todo | partial | done | n/a`); regenerate new
  entries with `uv run --no-project python -I scripts/upstream_map.py`, which keeps existing ones.
- **Ported upstream tests** live in `crates/openexr-core/src/upstream_tests/<file>.rs`, one module
  per upstream test file (they reach internal functions, as upstream's do), and carry a marker
  directly above `#[test]`:
  ``/// Port of `testReadScans` (src/test/OpenEXRCoreTest/read.cpp:257-403) @ v3.5.3.``
- **Headers.** Every `.rs` file starts with `// SPDX-License-Identifier: BSD-3-Clause` and
  `// Copyright Contributors to the OpenEXR Project.` Ports of vendored code keep their own
  license line (MIT for libdeflate, BSD for zstd, BSD-2-Clause for OpenJPH) and copyright.
  Ports of files whose upstream header credits DreamWorks (`internal_dwa.c`,
  `internal_dwa_table*.c`, `internal_b44_table*.c`) use
  `// Copyright (c) DreamWorks Animation LLC and Contributors of the OpenEXR Project.`
  (owner, 2026-11-07, `PLAN.md` O6); the DWA chunk adds `PATENTS` to
  `THIRD-PARTY-NOTICES.md`.
- **Dependencies.** Pinned exactly in the root `[workspace.dependencies]` (`=x.y.z`); a new
  dependency needs a reason and the owner's OK.
- **Errors.** Upstream's error codes and message text are output: copy them verbatim.
- **Non-ASCII data.** Write it as escapes in source, never raw; check with `od -c`.

## The oracle and the upstream build

- `oracle/` is a uv project pinned to `openexr==3.5.3` and `numpy==2.3.5`, Python 3.13. Run it
  with `uv run --project oracle python -I ...`. `oracle/smoke.py` is its first check.
- The oracle reports what the library does and never computes expected values.
- Upstream builds locally out of tree under `target/`, with the wheel's dependency settings:
  - Windows: `avr --non-interactive --run-as-tool --no-validation -p rez run rez-env msvc-14.44.35207 -- cmd /c scripts\upstream-build.cmd Release`
  - Rocky Linux 9: `scripts/rocky9.sh scripts/upstream-build.sh Release`
  - cmake and ninja come from PyPI through `uvx` (pinned in the scripts); use uv, never pip.
- Results of record are in `docs/baseline.md`: keep it current when the pins change.

## Commands

| Command | Purpose |
|---|---|
| `scripts/upstream-build.cmd [Release\|Debug] [ctest args]` | Build upstream Core and its tests on Windows (inside the MSVC rez environment), run the 73 Core tests |
| `scripts/rocky9.sh <cmd>` | Run a command in the Rocky Linux 9 reference container, checkout at `/work` |
| `scripts/upstream-build.sh [Release\|Debug] [ctest args]` | The same build and tests on Linux, inside the container |
| `uv run --project oracle python -I oracle/smoke.py <out> [images]` | The oracle's smoke check |
| `uv run --no-project python -I scripts/upstream_map.py` | Add new upstream files to `upstream-map.toml` |
| `cargo xtask gate` | Fast tier: fmt, clippy, guards, tests (every chunk) |
| `cargo xtask gate --full` | Adds the heavy tier: `--features heavy` tests (deferred, `PLAN.md` §8) |
| `cargo xtask guards` | Headers, `unsafe` allowlist, forbidden patterns, test markers, `upstream-map.toml` |
