# OpenEXR-rs

An exact Rust port of [OpenEXR](https://github.com/AcademySoftwareFoundation/openexr) **3.5.3**,
beginning with OpenEXRCore, accepted only on byte-by-byte parity with the official build.

## Status

**OpenEXRCore 3.5.3 is ported** (milestone M3, 2026-11-08), with the three libraries it
bundles:

| Crate | What it is | Rust lines |
|---|---|---|
| `openexr-rs-core` (`crates/openexr-core`) | OpenEXRCore: contexts and streams, every attribute type, header parsing and writing, chunk tables, decoding and encoding pipelines, deep data, `exr_print_*`, and every codec: NONE, RLE, ZIPS, ZIP, PIZ, PXR24, B44, B44A, DWAA, DWAB, HTJ2K256, HTJ2K32, LJ2K, ZSTD | 38,000 |
| `openexr-rs-deflate` (`crates/openexr-deflate`) | libdeflate 1.26: the decompressor and the compressor at every level, 0 to 12 | 6,200 |
| `openexr-rs-zstd` (`crates/openexr-zstd`) | zstd 1.5.7: the decompressor and the compressor at every level, 1 to 22, every strategy, the block splitter, long distance matching and index overflow correction | 13,500 |
| `openexr-rs-openjph` (`crates/openexr-openjph`) | OpenJPH 0.32.0: the HTJ2K decoder and encoder, the wavelets, colour transforms and codestream | 22,600 |
| `openexr-testkit` (`crates/openexr-testkit`) | Test-only: the platform C runtime and the CPU as references | 450 |
| `openexr-rs` (`crates/openexr`, library `openexr`) | **In progress (M4):** a safe Rust API over the Core port, with the C++ library's names. Done: opening files (a path, a `Read + Seek`, bytes), every part's `Header` and `Attribute`s, and reading flat parts' pixels into the caller's buffers (`FrameBuffer`, `Slice`, `InputPart`: scanlines, tiles, levels, line ranges, interleaved and subsampled channels, fill values), as the wheel reads them | 1,900 |

Every SIMD path upstream has is ported (ZIP's SSE2/SSE4.1, DWA's SSE2/AVX/F16C, libdeflate's
BMI2 decoder and vector Adler-32, zstd's BMI2 builds and SSE2 row search, OpenJPH's SSSE3, AVX2
and AVX-512 kernels), each chosen as the wheel's build chooses it, so the port computes what the
official build computes on each platform and CPU.

Core's API is its C API in Rust form (contexts, chunk infos, decode and encode pipelines). The
milestone now under way, M4 (`PLAN.md` §16), is a safe Rust API on top of it, `openexr-rs`:
headers and flat reads today, then deep data, writing and threads. The OpenEXR C++ library itself
comes later, as far as its features are wanted (M5).

## How parity is checked

Expected values come only from the official build, upstream's own tests, or the platform; never
from the port's own output (`CLAUDE.md`, rule 2). Comparisons are exact: pixels bit for bit,
files and chunks byte for byte, messages character for character.

- **The oracle:** the official `openexr==3.5.3` wheel (`oracle/`, pinned with uv). Every
  layout, channel type, sampling, tiling and compression the wheel writes decodes to its pixels
  and encodes to its bytes, headers and chunk tables included.
- **The source-built harness** (`oracle/native`): Core 3.5.3 itself, built with the wheel's
  settings, for what the wheel cannot reach: Core's header writer and packer, every codec level,
  Core's messages and `exr_print_context_info`, every half, float and uint conversion over all
  2^32 inputs, zstd frames past 3.5 GB. It is accepted only where it reproduces the wheel.
- **Upstream's tests:** all 73 OpenEXRCoreTest tests, 71 ported verbatim and 2 not applicable
  (they only exercise NULL contexts and C struct versioning).
- **The corpus:** [openexr-images](https://github.com/AcademySoftwareFoundation/openexr-images)
  at a pinned commit, plus damaged copies of every image (cut, overwritten, with impossible chunk
  sizes), read, decoded and rewritten as the wheel and Core do.
- **Platforms and CPUs:** Windows x86-64 (MSVC wheel) and Rocky Linux 9 x86-64 (GCC wheel),
  whose builds differ in places the port follows (DWA, OpenJPH's fused lifting, ATK shifts:
  `docs/baseline.md`); and, under Intel SDE, Nehalem, Sandy Bridge, Ivy Bridge, Haswell and
  Skylake-SP on both platforms.

Where upstream has a bug that changes outputs, the port copies it and logs it
(`docs/improvements.md`: 33 `I-` entries for behaviour copied, 22 `U-` entries for undefined
behaviour the port refuses instead). The owner approved 21 deviations (`docs/deviations.md`),
mostly corrected error text and writer bugs, with 17 waivers for the outputs they change
(`docs/waivers.md`); pixels and file bytes stay bit-exact.

## Speed

Against Core itself (the harness, the wheel's build settings), single-threaded, on Windows: the
port's time over Core's, decoding and encoding three corpus images rewritten into each
compression (`scripts/bench.py`, `docs/baseline.md`, "Speed"):

| | NONE | RLE | ZIPS | ZIP | PIZ | PXR24 | B44 | B44A | DWAA | DWAB | HTJ2K256 | HTJ2K32 | LJ2K | ZSTD |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Decode | 1.02 | 0.38 | 0.91 | 0.80 | 0.89 | 0.82 | 0.42 | 0.41 | 0.78 | 0.79 | 1.03 | 0.96 | 1.00 | 0.64 |
| Encode | 0.38 | 0.74 | 0.99 | 0.99 | 0.85 | 1.03 | 0.69 | 0.66 | 0.84 | 0.86 | 0.88 | 0.94 | 0.91 | 1.03 |

On Linux, against GCC's build, ZSTD stays slower (encoding about 1.2 to 1.35 times, decoding
about 1.4 to 1.6 in CPU time, measured before the buffers below): `docs/deferred.md`. The
pipelines' buffers are reused without clearing where upstream leaves its `malloc` uninitialized
(heavy-tier builds poison them, so a read before a write would change a checked output).

## Building and testing

The toolchain is pinned in `rust-toolchain.toml` (Rust 1.99.1). Everything runs locally.

| Command | What it does |
|---|---|
| `cargo xtask gate` | The fast tier: fmt, clippy, the guards (headers, `unsafe` allowlist, citations, `upstream-map.toml`) and the unit and ported upstream tests |
| `cargo xtask gate --full` | Adds the heavy tier: every check against the wheel, the harness, the corpus and the exhaustive sweeps |
| `scripts/rocky9.sh cargo xtask gate --full` | The same on Rocky Linux 9, in Docker |
| `scripts/sde.sh <cpu> cargo test ...` | A test run on an emulated CPU (nhm, snb, ivb, hsw, skx) |
| `uv run --no-project python -I scripts/bench.py` | The speed comparison with Core |

The heavy tier needs the oracle (`uv run --project oracle python -I oracle/smoke.py`), the
harness (`scripts/oracle-native-build.cmd` on Windows, in the MSVC rez environment, or
`scripts/oracle-native-build.sh` on Linux) and the corpus (`scripts/fetch-corpus.sh`).
`CLAUDE.md` lists every command.

## Where things are

- `PLAN.md`: the plan, the definition of done (§3), and the owner's decisions (§14).
- `CLAUDE.md`: the rules the port is written by.
- `upstream-map.toml`: every upstream file in scope, its Rust target and its status.
- `docs/baseline.md`: results of record (upstream's tests, the oracle, the harness, Rocky Linux,
  SDE, speed).
- `docs/improvements.md`, `docs/deviations.md`, `docs/waivers.md`: upstream bugs, approved
  deviations and their waivers.
- `docs/deferred.md`: what is left for later.
- `docs/m3.md`: the Core parity milestone, what it covers and how it was checked.
- `upstream/`: OpenEXR v3.5.3 and Imath v3.2.3 as submodules.

## License

BSD-3-Clause, as upstream (`LICENSE`, `NOTICE`). The translated libdeflate (MIT), zstd (BSD),
OpenJPH (BSD-2-Clause) and Imath (BSD-3-Clause) code keeps its own license, and DWA comes with
DreamWorks' patent grant: `THIRD-PARTY-NOTICES.md`.
