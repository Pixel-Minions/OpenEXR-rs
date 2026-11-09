# OpenEXR-rs: porting plan

**Status:** v0.2, 2026-11-07. Porting OpenEXRCore has started (owner, 2026-11-07: "lets
start with core"), with a lean harness: the fast test tier runs on every chunk and the heavy
tier waits until Core is in place (§8, "Test tiers"). Still local only: no CI, no profiling, no
pushing (owner, 2026-11-06). Section 14 records what the owner has decided and what is open.

**Target:** OpenEXR **3.5.3** exactly (`upstream/openexr`, tag `v3.5.3`, commit
3950290530c4fd4776667d0dc4d65a9d003eb936), the latest release. The first section is
**OpenEXRCore**, the C library, with the code vendored into it.

**The model:** OpenColorIO-rs (`D:\Projects\OpenColorIO-rs`, `PLAN.md` and `CLAUDE.md`), which
ported OCIO 2.5.2 to byte-for-byte parity with agents. This plan carries over its definition
of done, its way of working and its verification, and changes only what OpenEXR makes
different. Where this plan is silent, OpenColorIO-rs's rule applies.

**Contents:**
1. [Summary](#1-summary)
2. [Target and versioning](#2-target-and-versioning)
3. [Definition of done](#3-definition-of-done-byte-by-byte)
4. [What we are porting](#4-what-we-are-porting-openexrcore-353)
5. [Decisions](#5-decisions)
6. [Consumers](#6-consumers)
7. [How agents do the work](#7-how-agents-do-the-work)
8. [Verification](#8-verification)
9. [Architecture](#9-architecture)
10. [Roadmap](#10-roadmap)
11. [Effort and calendar](#11-effort-and-calendar)
12. [Porting later OpenEXR versions](#12-porting-later-openexr-versions)
13. [Risks](#13-risks)
14. [Decisions for the owner](#14-decisions-for-the-owner)
15. [Next steps](#15-next-steps)

---

## 1. Summary

- **What.** A Rust port of OpenEXR 3.5.3 that behaves exactly like it, beginning with
  OpenEXRCore: the same pixels, the same file bytes, the same accepted and refused files, the
  same error codes and messages. A section is accepted only on byte-by-byte checks against the
  real OpenEXR 3.5.3 (the oracle) plus upstream's own tests.
- **Size of OpenEXRCore 3.5.3** (non-blank lines; `docs/baseline.md` has the method):
  - 36.8K lines of C and C++ (1.6K of it C++, the HTJ2K glue) in 92 files, plus its CMake file;
  - 184 public functions in 15 public headers (3.5K lines);
  - 14 compression methods, 4 storage types (scanline, tiled, deep scanline, deep tiled);
  - three vendored libraries compiled into it: libdeflate v1.26 (8.9K lines reached),
    zstd v1.5.7 (41.5K) and OpenJPH 0.32.0 (43.9K, of which much is per-CPU variants);
  - Imath 3.2.3's `half.h` conversions, which upstream fetches rather than vendors;
  - upstream's Core tests: **73** (`src/test/OpenEXRCoreTest`), all passing on both reference
    platforms in Release and on Windows in Debug (`docs/baseline.md`).
- **How.** The same as OpenColorIO-rs: a clean port from source into Rust, one upstream file at
  a time, mirroring upstream's layout; each function cites its upstream lines; expected values
  come only from the oracle, upstream's tests or the platform; comparisons are bitwise.
- **Byte-exact is achievable, with one platform split already measured.** The official wheels
  write byte-identical files on Windows and Linux for all 14 codecs, and decode identical
  pixels for the other 12. DWAA and DWAB decode 2 of 65,536 half values differently (one bit),
  because the Linux build runs AVX/F16C paths that MSVC compiles out. The port reproduces both
  (D12).
- **Effort** (§11). About 89 person-weeks of conventional work for OpenEXRCore with exact ports
  of the three vendored libraries, or about 52 without them. At the pace OpenColorIO-rs
  measured with agents, that is weeks, not months; the conventional range is 4–7 months.

---

## 2. Target and versioning

The scheme of OpenColorIO-rs (its §2), with OpenEXR in place of OCIO:

| Item | Scheme |
|---|---|
| Our version | Our own semver, `0.x` while porting, with the matched release as build metadata: `0.1.0+openexr.3.5.3` |
| Upstream pin | `upstream/openexr` at `v3.5.3`; oracle `openexr==3.5.3`; Imath pinned at `v3.2.3` (Phase 0) |
| Runtime version | `exr_get_library_version` and its port report 3.5.3, as upstream does; the port's own version is a separate constant |
| Later releases | A new OpenEXR minor is a major bump of ours; a patch is a minor bump (§12) |
| Distribution | Nothing is published. The names in §9 are free on crates.io today (2026-11-06) |

What 1.0.0 means (OpenEXRCore alone, or Core plus the C++ `Imf` library and the Python module)
is decided when Core is done.

---

## 3. Definition of done: byte by byte

A section ships only when all four hold. For OpenEXRCore:

**1. Upstream's tests pass.** Every one of the 73 OpenEXRCoreTest tests is ported and passes,
or is on a reviewed not-applicable list. Upstream checks that compare Core with the C++ library
(`compression.cpp`, `deep.cpp`, `base_units.cpp` link `Imf`) compare with the oracle instead.

**2. Output is byte-exact against the oracle.**

| Output | Must be identical |
|---|---|
| Written files and compressed chunks, every codec and level; header bytes and chunk tables; parsed header contents and attribute values; which files are accepted and refused; error codes, and error and warning text; `exr_print_*` text | Byte for byte, on every platform |
| Decoded pixels, every codec, pixel type, sampling, storage and level; deep sample counts and samples | Bit for bit against the oracle on the same OS and CPU. The reference platforms are Windows x86-64 and Linux x86-64 (Rocky Linux 9) |

**3. The corpus matches.** Every file of `openexr-images` and of upstream's test directories
opens or fails exactly as in the oracle, with the same headers and the same pixels.

**4. No unapproved waivers.** Every exception is in `docs/waivers.md` with its cause and the
owner's sign-off.

### Why "same OS and CPU"

OpenEXRCore gives different bits on different machines itself:
- **DWA.** `internal_dwa_simd.h` has scalar, SSE2, AVX (inline assembly), F16C and NEON paths,
  chosen by `check_for_x86_simd` (`internal_cpuid.h`) at run time. Under MSVC the CMake probe
  `OPENEXR_IMF_HAVE_GCC_INLINE_ASM_AVX` fails, `xgetbv` is never read, and AVX and F16C always
  read as absent: the Windows wheel never takes those paths. Measured: 2 of 65,536 values
  differ between the wheels on this machine (`docs/baseline.md`).
- **OpenJPH.** It chooses SSE through AVX-512 kernels at run time (`ojph_arch.cpp`). The
  reversible (lossless) path is integer arithmetic; the irreversible path (LJ2K) uses a float
  9/7 wavelet, whose per-kernel results are not yet proven equal.
- **Integer SIMD** (the ZIP predictor and interleave in `internal_zip.c`, zstd's shuffle and
  delta, libdeflate's checksums and matchfinder setup, zstd's BMI2 decoders) gives the same
  results on every CPU by construction; Phase 0 checks this once.

So the port does what OpenEXR does (D12): where OpenEXR is byte-identical across platforms and
CPUs, so is the port; where it differs, the port differs the same way.

### How we get bit-exact results

The rules of OpenColorIO-rs, translated from C++ to C (`CLAUDE.md`, "Bit-exact porting"):
- the same operation order and float/double boundaries, including C's usual arithmetic
  conversions and integer promotions (`uint16_t * uint16_t` is an `int` multiply);
- C integer semantics: unsigned arithmetic wraps; shifts, signed overflow and out-of-range
  float-to-int conversions are undefined in C, and the port does what the wheel's machine code
  does or refuses (OpenColorIO-rs's `U-`/`I-` rule);
- the platform's own `printf` for the 78 `%g` conversions in messages and debug output
  (OpenColorIO-rs's `cfmt` approach);
- each SIMD path as its own kernel, dispatched exactly as upstream dispatches, MSVC quirk
  included.

---

## 4. What we are porting (OpenEXRCore 3.5.3)

Non-blank lines at `v3.5.3`. Difficulty runs from 1 (mechanical) to 5 (subtle).

| Area | Upstream files | Lines | Difficulty | Notes |
|---|---|---:|:---:|---|
| **Public API** | `openexr*.h` (15) | 3,471 | 2 | 184 functions; enums, structs, error codes. Layout of the Rust API is an owner item (D2) |
| **Base and plumbing** | `base.c`, `context.c`, `memory.c`, `internal_structs.*`, `internal_file.h`, `internal_{posix,win32}_file_impl.h`, `internal_xdr.h`, `internal_cpuid.h`, `internal_thread.h`, `debug.c`, … | 3,546 | 2–3 | Contexts, user allocators and streams, error handler, XDR byte order, CPU detection, `exr_print_*` |
| **Attributes** | `attributes.c`, `string.c`, `string_vector.c`, `float_vector.c`, `channel_list.c`, `opaque.c`, `bytes.c`, `preview.c`, `std_attr.c`, `color_metadata.c` | 3,103 | 2 | Typed attributes, opaque handlers, color interop IDs |
| **Header parse, parts, validation** | `parse_header.c`, `part.c`, `part_attr.c`, `validation.c` | 6,237 | 4 | 475 error sites in Core report through the error handler; their text is part of the output |
| **Chunk table and read** | `chunk.c` | 2,149 | 4 | Chunk-table reconstruction for damaged files, deep chunks |
| **Coding pipeline** | `coding.c`, `decoding.c`, `encoding.c`, `unpack.c`, `pack.c`, `compression.c` | 4,199 | 3–4 | `unpack.c` has many specialised unpackers chosen by layout; `compression.c` includes libdeflate |
| **Header writer** | `write_header.c` | 620 | 2 | Not reached by the wheel (§8) |
| **RLE** | `internal_rle.c` | 196 | 1 | |
| **ZIP, ZIPS** | `internal_zip.c` | 381 | 2 | SSE2/SSE4.1 predictor and interleave (integer) |
| **PIZ** | `internal_piz.c`, `internal_huf.c` | 2,377 | 4 | Wavelet plus Huffman; fast and slow Huffman decoders |
| **PXR24** | `internal_pxr24.c` | 370 | 2 | Float to 24 bits, then deflate |
| **B44, B44A** | `internal_b44*.c` | 704 | 3 | Tables built at init |
| **DWAA, DWAB** | `internal_dwa*.c`, `internal_dwa_*.h` | 5,978 | 5 | DCT, quantisation, classifier; SIMD paths in `internal_dwa_simd.h` (2,063), per CPU and platform |
| **ZSTD** | `internal_zstd*.c` | 1,868 | 3 | Channel shuffle and delta transforms around zstd |
| **HTJ2K, LJ2K** | `internal_ht*.cpp`, `internal_ht*.h` | 1,557 | 3 | C++ glue to OpenJPH |
| **libdeflate v1.26** | `external/deflate/lib` (the files `compression.c:43-53` includes) | 8,879 | 4 | Compressor (hash-chain, binary-tree, near-optimal parsers by level), decompressor, Adler-32, zlib wrapper |
| **zstd v1.5.7** | `external/zstd/lib` (26 sources, `CMakeLists.txt:156-180`) | 41,465 | 5 | Every strategy from `fast` to `btultra2` is reachable through the level; decompressor; FSE, Huffman, xxHash |
| **OpenJPH 0.32.0** | `external/OpenJPH/src/core` | 43,943 | 5 | HT block coder, codestream, 5/3 and 9/7 wavelets, colour transforms; generic plus SSE…AVX-512 variants |
| **Imath 3.2.3 `half.h`** | `imath_half_to_float`, `imath_float_to_half` (C, no lookup table) | ~150 | 2 | `internal_coding.h:15-24` |
| **Tests** | `src/test/OpenEXRCoreTest` (25 files) | 80.5K raw | — | 73 tests; `dwaLookups.h` and `dwaQuantTables.h` are 69.5K lines of generated tables |

`upstream-map.toml` accounts for every one of these files (324 entries: OpenEXRCore, its tests,
the 6 images they open, and every vendored file), each `todo` or `n/a` with a reason.

**Not in this section:** the C++ libraries (`OpenEXR`/`Imf`, `Iex`, `IlmThread`, `OpenEXRUtil`),
the Python binding, the tools, the website and docs. They are planned when Core is done.

---

## 5. Decisions

Carried over from OpenColorIO-rs, adjusted to OpenEXR:

| # | Decision | Choice | Status |
|---|---|---|---|
| **D1** | Strategy | A clean port from source, checked against the oracle | Carried over |
| **D2** | API surface | A Rust-shaped mirror of Core's C API: an owned `Context`; each C function keeps its name without `exr_` and returns `Result<T, Error>`, where the error carries the `EXR_ERR_*` code and exactly the message upstream's error handler receives; the optional error-handler callback is called as upstream calls it; streams are traits; out-parameters become return values | **Decided** (owner, 2026-11-07) |
| **D3** | Target | OpenEXR 3.5.3 exactly | Decided |
| **D4** | Equivalence | Byte-exact as in §3; waivers only with the owner's sign-off | Carried over |
| **D7** | Toolchain | Owner item O5 (§14) | Open |
| **D8** | `unsafe` | Denied across the workspace; allowed only in the files of the allowlist `xtask guards` enforces: SIMD kernels, the test-only C runtime reference (`openexr-testkit/src/crt.rs`) and CPU reference (`openexr-testkit/src/cpu.rs`), and the one module that asks the OS for its error text (`os_error.rs`) | **Decided** (owner, 2026-11-07, O7-O9) |
| **D9** | License | BSD-3-Clause with upstream's notice; vendored libraries keep their own (MIT, BSD, BSD-2) in `THIRD-PARTY-NOTICES.md`; DWA carries upstream's `PATENTS` terms | Required |
| **D11** | Platforms | Windows x86-64 and Linux x86-64 (Rocky Linux 9.8, glibc 2.34); ARM and macOS later | Carried over |
| **D12** | Output across platforms | Do what OpenEXR does on each platform and CPU | Carried over |
| **O1–O5** | Oracle, vendored libraries, SIMD, crate layout, toolchain | §14 | **Owner** |

---

## 6. Consumers

**recon** (`D:\BarnstormGitlab\recon`) is the first. It decodes a 6K DWAB plate at 8–11 fps
through the `exr` crate (exrs, a fork pinned by git revision), where DJV, which decodes through
OpenEXR, plays it at 18–19 fps. OpenEXR-rs should replace exrs there, which needs reading
(Milestone M0) and later speed. Speed is deliberately not part of this plan yet: profiling comes
after the port is exact (owner, 2026-11-06). As in OpenColorIO-rs, the consumer aligns with the
port, never the other way around.

---

## 7. How agents do the work

As in OpenColorIO-rs (its §7 and `CLAUDE.md`), unchanged in substance:
- **Port, don't reinvent.** Every Rust function translates a named upstream function and cites
  `file:lines @ v3.5.3` (or `@ v1.26`, `@ v1.5.7`, `@ 0.32.0`, `@ v3.2.3` for the vendored and
  fetched code). Upstream bugs that change output are ported and listed in
  `docs/improvements.md`.
- **The oracle decides.** Expected values come only from the oracle, upstream's tests (copied
  verbatim, with a citation) or the platform. Never from the port.
- **Small chunks.** One upstream file or module per chunk with its tests, under ~600 lines of
  non-test code, green on its own. Oracle changes stand alone and are reviewed by an
  independent verifier agent.
- **Guardrails:** locked fixtures, a ratchet on ported upstream tests, no `#[ignore]`/`todo!()`,
  exact comparisons, the `unsafe` allowlist, pinned dependencies, headers, the upstream map.
- **Team:** 2–3 agents at a time: one or two implementers along the dependency order of §10, and
  a verifier who reviews each chunk adversarially against upstream.
- **For now, everything is local.** No CI, no pushes (owner, 2026-11-06). The gate runs on this
  workstation and in the Rocky Linux 9 container; `cargo xtask land` merges locally.

---

## 8. Verification

### The oracle: the official wheel

`oracle/` is a uv project pinned to `openexr==3.5.3` and `numpy==2.3.5` on Python 3.13. The
smoke check proves on both platforms that it reads and writes files and that every lossless
path returns the bits it was given (`docs/baseline.md`).

What the wheel can show, and what it cannot (from the 3.5.3 sources):
- **Reaches Core:** header parsing and validation (every file is opened through
  `exr_start_read`), the chunk table, decoding and unpacking, every decompressor, and every
  compressor at every level (`ImfCompressor.cpp` runs `exr_compress_chunk` in a temporary Core
  context). Files the wheel writes therefore carry Core's compressed chunks.
- **Does not reach Core:** Core's header writer (`write_header.c`, `exr_start_write`), packing
  (`pack.c`, `exr_encoding_run`), Core's API-level errors and their messages, `exr_print_*`,
  and Core's own behaviour on API misuse. The C++ writer, not Core, chooses the header bytes of
  files the wheel writes.
- **Only through Python:** the extension exports nothing but `PyInit_OpenEXR`.

Whether to add a source-built harness for what the wheel can't show is owner item **O1**.

### Test tiers

Owner, 2026-11-07: "delay heavy tests to when all is set so we speed up things".
- **Fast tier, every chunk** (`cargo xtask gate`): fmt, clippy, guards and `cargo test` on
  Windows: unit tests and ported upstream tests that run in seconds. Expected values still come
  only from upstream's tests, the oracle or the platform.
- **Heavy tier, deferred** (`cargo xtask gate --full`, once Core's read path is in place, at
  the latest before each milestone of §10): the Rocky Linux 9 run, oracle fixtures and the
  corpus, exhaustive sweeps (every float for half conversion, every codec level), the
  source-built harness (O1), and SDE. Heavy tests are written with the chunk that needs them,
  behind the `heavy` feature, so they run only in `--full`. Nothing is weakened: a chunk is *done* only after its heavy tests pass; until then
  `upstream-map.toml` marks it `partial`.
- **The cost:** a mismatch the heavy tier finds is found later, so it may touch several chunks.

### Upstream's tests

The 73 OpenEXRCoreTest tests are ported, marked ``/// Port of `testFoo` (src/test/OpenEXRCoreTest/file.cpp:lines) @ v3.5.3.``
and counted by the ratchet. They run against the upstream build locally today
(`scripts/upstream-build.cmd`, `scripts/upstream-build.sh`); `docs/baseline.md` records the
result on each platform.

### Probe inputs

- Every half bit pattern; random float and uint bit patterns, NaN payloads and infinities.
- Every codec and level (ZIP 0–9, DWA levels, ZSTD levels, HTJ2K qualities), every storage
  type, line order, tile size and level mode, subsampled channels, odd sizes and data windows.
- Damaged files: truncated chunks, bad offsets, bad attribute sizes (upstream's own broken test
  images, then generated mutations through the oracle).

### Corpora

- Upstream's test images (`src/test/OpenEXRTest`, `src/wrappers/python/tests`).
- `openexr-images` (AcademySoftwareFoundation/openexr-images at
  e38ffb0790f62f05a6f083a6fa4cac150b3b7452), fetched by a script into `corpus/`, not committed.
- Studio plates, locally only (the 6K DWAB plate of recon among them).

### Emulated CPUs

As in OpenColorIO-rs: `scripts/sde.sh <cpu>` runs the oracle and the port on an emulated CPU,
for every SIMD path the Linux wheel dispatches (`nhm` SSE2 only; `snb` AVX without F16C; `ivb`,
`hsw`, `skl` AVX and F16C; `skx` and later AVX-512 for OpenJPH). Locally only, by hand.

---

## 9. Architecture

Owner item **O4** decides names. The recommended layout:

```
OpenEXR-rs/
├── Cargo.toml              workspace: lints, toolchain, exact dependency pins
├── crates/
│   ├── openexr-core/       openexr-rs-core: port of src/lib/OpenEXRCore
│   ├── openexr-deflate/    openexr-rs-deflate: port of external/deflate (libdeflate v1.26)
│   ├── openexr-zstd/       openexr-rs-zstd: port of external/zstd (zstd v1.5.7)
│   ├── openexr-openjph/    openexr-rs-openjph: port of external/OpenJPH (0.32.0)
│   └── openexr-testkit/    dev-only: oracle client, exact comparators, corpus runner
├── oracle/                 uv project pinned to openexr==3.5.3
├── fixtures/               generated by the oracle; hash manifest; read-only for agents
├── upstream/openexr/       submodule @ v3.5.3
├── upstream/imath/         submodule @ v3.2.3 (Phase 0; half.h)
├── xtask/                  gate, land, guards, upstream map, oracle regen, parity
├── upstream-map.toml       every upstream file → its Rust module (or n/a)
└── docs/                   baseline, parity, waivers, deviations, improvements, cards
```

- Library names `openexr_core`, `openexr_deflate`, `openexr_zstd`, `openexr_openjph`, so they
  never collide with `exr` (exrs), which recon still uses.
- One crate per vendored library: each keeps its own license, upstream version and tests, and
  Core depends on it as upstream's CMake does.
- Later sections add `openexr` (`openexr-rs`, the C++ `Imf` port) and the Python module.

### C idioms → Rust

| C (upstream) | Rust (port) |
|---|---|
| `exr_context_t` (opaque pointer), `exr_result_t` codes | An owned `Context`; `Result<T, ExrError>`, where `ExrError` carries the `EXR_ERR_*` code and the handler's message verbatim |
| User allocators (`alloc_fn`/`free_fn`), read/write/size callbacks | Traits for streams; allocation through Rust (an allocator hook only if a consumer needs one) |
| Error handler callback, `snprintf` messages | The same callback signature; messages formatted as the platform's `printf` formats them |
| `void*` buffers with strides (`exr_coding_channel_info_t`) | Typed slices plus strides; the unpacker choice ported as upstream makes it |
| Function-pointer pipelines (`decode_fn`, `unpack_and_convert_fn`) | Function values chosen by the same rules |
| Platform file I/O (`internal_posix_file_impl.h`, `internal_win32_file_impl.h`) | `std::fs` with the same behaviour; Windows UTF-8 path handling ported |
| Thread-local zstd contexts, once-initialised tables (B44, DWA) | `thread_local!`, `OnceLock` |

---

## 10. Roadmap

The order follows upstream's own layering and the owner's order: base and attributes, header
parse, chunk table and read, unpack, then each codec, then write and encode, then deep. Each
codec lands whole (compressor and decompressor are in the same upstream file), checked through
the wheel in both directions. The vendored library each codec needs lands with it.

| Phase | Scope | Exit |
|---|---|---|
| **0 Harness, lean** (~1 pw now) | Workspace and toolchain (O5); `xtask` gate (fast tier) and guards; Imath submodule. Deferred to the heavy tier (§8): `openexr-testkit` with the oracle protocol, fixtures, corpus fetch, Rocky gate, the source-built harness (O1), SDE, the CPU-independence check | `cargo xtask gate` green on Windows with zero ported code |
| **1 Foundations** (~4 pw) | Public types and error codes, `exr_get_error_code_as_string`, memory, context lifecycle, streams and file I/O, XDR, CPU detection, half conversion, `base.c` | Ported base tests (`testBase*`, `testCPUIdent`, `testHalf`, `testXDR`, `testTempContext`, `testInitializerCompat`) |
| **2 Attributes** (~3 pw) | Every attribute type, opaque handlers, `std_attr.c`, `color_metadata.c` | `testAttr*`, `testColor*` |
| **3 Header parse** (~5 pw) | `parse_header.c`, `part.c`, `part_attr.c`, `validation.c` | Every corpus header parses like the oracle; refused files refused with the same message |
| **4 Chunks and read** (~4 pw) | `chunk.c`, `coding.c`, `decoding.c`, `unpack.c`, `compression.c` dispatch; NONE | `testRead*`, `testOpen*`, `testSamplingCalcs`; uncompressed files decode bit-exactly |
| **5a RLE, ZIP, ZIPS + libdeflate** (~7 pw) | `internal_rle.c`, `internal_zip.c` (with its SSE2/SSE4.1 paths), libdeflate's decompressor then compressor (every level) | Chunks byte-identical to the wheel's at every ZIP level |
| **5b PIZ** (~2.5 pw) | `internal_piz.c`, `internal_huf.c` | `testHUF`, `testPIZCompression` |
| **5c PXR24, B44, B44A** (~1.5 pw) | `internal_pxr24.c`, `internal_b44*.c` | `testPXR24*`, `testB44*`, `testB44Table` |
| **5d DWAA, DWAB** (~8 pw) | `internal_dwa*`, scalar and SSE2, then the AVX/F16C paths with the upstream dispatch | `testDWA*`; DWA pixels bit-exact on each platform and on each emulated CPU. **Milestone M0:** read every codec but ZSTD and HTJ2K |
| **5e ZSTD + zstd** (~17.5 pw) | `internal_zstd*.c`; zstd's decompressor, then its compressor (every strategy) | `testZstd*`, `testDeepZstd*`; chunks byte-identical at every level |
| **5f HTJ2K, LJ2K + OpenJPH** (~15.5 pw) | `internal_ht*`; OpenJPH decoder, then encoder; generic kernels first, then each SIMD variant | `testHT*`; lossless and lossy chunks byte-identical, LJ2K pixels per CPU. **Milestone M1:** every codec, both directions |
| **6 Write and encode** (~3 pw) | `write_header.c`, `encoding.c`, `pack.c`, `exr_start_write`, update-in-place | `testWrite*`, `testStartWrite*`, `testUpdateMeta`. **Milestone M2:** Core writes files |
| **7 Deep** (~2 pw) | Deep scanline and tiled reading and writing across chunk, decode, unpack | `testOpenDeep`, `testReadDeep`, `testWriteDeep`, `testDeep*Compression` |
| **8 Remaining API and SIMD** (~3 pw) | `debug.c` (`exr_print_*`), remaining getters and setters, the last SIMD profiles under SDE | All 73 tests; corpus green. **Milestone M3:** OpenEXRCore 3.5.3 parity |

Phases 1–4 are a strict chain. From Phase 5 on, the codecs are independent of each other and
can run in parallel (5d needs 5a's inflate; 5e and 5f need nothing but Phase 4); Phases 6 and 7
need Phase 4 and at least one codec.

---

## 11. Effort and calendar

Conventional person-weeks, from the line counts of §4 at ~1.5K non-blank C lines per
person-week for mechanical code and less for the subtle areas (DWA, PIZ, zstd, OpenJPH):

| Phase | pw |
|---|---:|
| 0 Harness | 5 |
| 1 Foundations | 4 |
| 2 Attributes | 3 |
| 3 Header parse | 5 |
| 4 Chunks and read | 4 |
| 5a RLE, ZIP + libdeflate | 7 |
| 5b PIZ | 2.5 |
| 5c PXR24, B44 | 1.5 |
| 5d DWA with SIMD | 8 |
| 5e ZSTD + zstd | 17.5 |
| 5f HTJ2K + OpenJPH | 15.5 |
| 6 Write and encode | 3 |
| 7 Deep | 2 |
| 8 Remaining API and SIMD | 3 |
| Integration (~10%) | 8 |
| **Total** | **~89** (range 75–110) |

- **Without exact ports of the vendored libraries** (O2, option B): libdeflate, zstd and
  OpenJPH drop to crate integration and waivers, about 52 pw in all. OpenJPH can't drop in that
  option without losing LJ2K pixel parity (§14, O2).
- **Calendar.** OpenColorIO-rs planned 133 pw and delivered Phases 0–9 between 2026-09-28 and
  2026-11-02 with 2–4 implementer agents plus verifiers: roughly 25 conventional pw per
  calendar week. At that pace OpenEXRCore takes about 4–5 weeks with the vendored ports (M0 in
  about 2), about 2–3 without. The conventional assumption of 3–5 pw per week gives 4–7
  months. Re-estimate after Phase 1.
- **The critical path** is the harness, exact error text, DWA per CPU, and the zstd compressor.

---

## 12. Porting later OpenEXR versions

As in OpenColorIO-rs §12: bump the submodule and the oracle, diff the tags, map the changed
files through `upstream-map.toml`, port the changed tests first. The three vendored libraries
move with upstream's `external/` (`current_deflate_version`, `current_zstd_version`,
`ojph_version.h`), so a later OpenEXR release can also mean a libdeflate, zstd or OpenJPH diff.

---

## 13. Risks

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| zstd's compressor is large and heuristic; byte parity at every level is long work | High | Medium | Land it last among the codecs (5e); decoder first; oracle chunks at every level |
| OpenJPH's float paths differ per CPU (LJ2K) | Medium | Medium | Measure under SDE in Phase 0 before 5f; each kernel its own profile |
| DWA differences beyond the measured 2 values, on other CPUs | Medium | Medium | SDE runs of the oracle in Phase 0; the DWA card ports each path separately |
| What the wheel can't show (header writer, API errors) goes unchecked | Certain without O1 | Medium | O1: a source-built harness, itself checked against the wheel |
| Agents game the checks | High | High | OpenColorIO-rs's guardrails, verifier, ratchet |
| Undefined behaviour in C (shifts, overflow, out-of-range casts) that the wheel resolves one way | Medium | Medium | Read the wheel's machine code; `U-`/`I-` entries in `docs/improvements.md` |
| Malicious files | Medium | High | No `unsafe` in parsers; upstream's own limits; fuzzing after parity |
| Patent terms of DWA (US 8,340,442 and 8,666,186, §14 O6) | Low | Medium | Decided O6: credit DreamWorks Animation and carry `PATENTS`; DWA used only inside EXR files |

---

## 14. Decisions for the owner

Each has a recommendation. Status on 2026-11-09:

| Item | Status |
|---|---|
| O1 Oracle | **Decided** (owner, 2026-11-07): B, the wheel plus a source-built Core harness, accepted only where it reproduces the wheel |
| O2 Vendored libraries | **Decided** (owner, 2026-11-07): A, port libdeflate, zstd and OpenJPH exactly, each as its own crate, in the stage that needs it |
| O3 SIMD | **Decided** (owner, 2026-11-07): A, every SIMD path upstream takes reproduced exactly in safe code and chosen as upstream chooses it; real SIMD kernels later, after profiling. **Then** (owner, 2026-11-08): profile and port the SIMD paths as real kernels, in the `unsafe` allowlist's files, each chosen as upstream chooses it (D-21 for the two taken on both platforms); done |
| O4 Layout and naming | Recommendation in use since the owner started the port (2026-11-07); open to change until anything is published |
| O5 Rust version | Recommendation in use since the owner started the port (2026-11-07); open to change |
| O6 DWA patents | **Decided** (owner, 2026-11-07): credit DreamWorks |
| O7 OS error text | **Decided** (owner, 2026-11-07): exact, through `FormatMessageA`/`strerror` |
| O8 What Rust can't have | **Decided** (owner, 2026-11-07): n/a, each with its reason |
| O9 `unsafe` in the test kit | **Decided** (owner, 2026-11-07): kept; and a CPU probe (`openexr-testkit/src/cpu.rs`), test-only, for the SIMD emulations |
| O10 The milestone after M3 | **Decided** (owner, 2026-11-09): C, a safe Rust API first (M4, §16), the C++ library later and only as far as its features are wanted (M5; `docs/proposal-m4.md`) |
| O11 M4's crate and names | **Decided** (owner, 2026-11-09): a crate `openexr-rs` (`crates/openexr`, library `openexr`), unpublished; the C++ library's names (`Header`, `FrameBuffer`, `Slice`, `InputPart`, ...) in Rust's case |
| O12 M4's threading | **Decided** (owner, 2026-11-09): `rayon`, as the C++ library uses IlmThread: single-threaded by default, a thread count per call (`threads(n)`, `threads(Auto)`) on request, behind a default `rayon` feature (without it, requests run single-threaded) |
| O13 Publishing | **Open**: whether and when any crate goes to crates.io (after M4's API is reviewed) |
| O14 M4's typed buffers | **Decided** (owner, 2026-11-09): `bytemuck` (`=1.25.2`, no default features) casts the caller's `&mut [f32]`, `&mut [u16]` and `&mut [u32]` to the bytes Core takes, without a copy and without `unsafe` in our crates |
| Upstream bugs found in Phases 2-4 (I-5 to I-12) | **Decided** (owner, 2026-11-07): fixed as deviations D-1 to D-8 (`docs/deviations.md`), with waivers W-1 to W-7 (`docs/waivers.md`); I-13 as D-9 with W-8; I-14 to I-16 (the header writer) as D-10 to D-12 with W-9 to W-11; I-17, I-18 (the chunk writes) as D-13, D-14 with W-12, W-13; I-19, I-20 (the encode pipeline) as D-15, D-16 with W-14, W-15; I-21 (tile chunk info) as D-17 with W-16; I-29, I-30 (the deep sample table, the temporary file's rename) as D-19, D-20, and U-21 (Core's write locks, not copied): accepted on review (owner, 2026-11-07) |

### O1 — Oracle

- **A. The official wheel only.** Covers decoding and every compressor through Core, header
  parsing, and refusals of damaged files. Doesn't cover Core's header writer, packing, Core's
  API errors and their messages, or `exr_print_*`; those would be checked only by the ported
  upstream tests.
- **B. The wheel, plus a source-built Core harness.** A small native program (C, built from the
  pinned submodule with the wheel's exact CMake settings, the same way `scripts/upstream-build.*`
  builds today) that exposes Core's C API to the oracle over the same request protocol, like
  OpenColorIO-rs's `oracle/native`. The wheel stays the authority: the harness is accepted only
  where it reproduces the wheel byte for byte (every corpus file decoded, every codec and level
  compressed, on both platforms), and is used only for what the wheel can't reach.

**Decided (owner, 2026-11-07): B.** The harness is used only for what the wheel can't reach, and
accepted only where it reproduces the wheel byte for byte.

**Recommendation: B.** Parts of Core (`write_header.c`, `pack.c`, most of `encoding.c`, the API
error paths, `debug.c`) can't be checked against the wheel at all, and its first consumer will write
files. Building it locally is already proven (73/73 on both platforms). Cost: about 1 pw of
Phase 0, and a native toolchain on each reference platform.

### O2 — Vendored libraries

Decoding needs only a correct inflate, zstd decoder and HTJ2K decoder, with one exception:
LJ2K's pixels come from OpenJPH's own float arithmetic. Encoding byte for byte needs exact
ports. The deflate and zstd outputs reach more codecs than their names suggest: PXR24 and DWA
compress with deflate too.

- **A. Port all three exactly** (libdeflate, zstd, OpenJPH), each as its own crate, in the
  codec phase that needs it. Every file and chunk byte-identical. ~36 pw of the ~89.
- **B. Rust crates** (an inflate/deflate crate, a zstd crate, and an HTJ2K decoder, of which
  none mature exists in pure Rust today). Decoded pixels stay identical except LJ2K. Encoded
  bytes differ for ZIP, ZIPS, PXR24, DWAA, DWAB, ZSTD, HTJ2K256, HTJ2K32 and LJ2K; only NONE,
  RLE, PIZ, B44 and B44A stay byte-identical. Each difference is a waiver.
- **C. Link the C libraries** through `-sys` crates. Exact bytes at once, but C in the build
  and `unsafe` FFI, which this port exists to remove. Not recommended.

**Recommendation: A, with libdeflate first** (5a: it serves ZIP, ZIPS, PXR24 and DWA, the
codecs studios use most), **OpenJPH next** (no Rust alternative, and LJ2K pixels need it),
**zstd last** (largest, newest codec). If the calendar matters more than encoder parity for
ZSTD, B for zstd alone is the least costly compromise: it saves ~14 pw and waives only ZSTD's
encoded bytes.

### O3 — SIMD

- **A. Per-CPU bit-exactness, as in OpenColorIO-rs.** Port the scalar path first, then each
  SIMD path upstream has on x86-64 (DWA SSE2, AVX, F16C; the ZIP predictor; zstd's transforms;
  OpenJPH's variants) as its own kernel, with upstream's dispatch, including the MSVC quirk that
  hides AVX and F16C on Windows. Check each path under SDE on the CPUs that select it, locally.
- **B. Scalar first, SIMD later.** Everything lands scalar; the SIMD paths come in a later
  phase. Until then, Linux results on AVX/F16C machines differ from the Linux wheel (measured:
  2 of 65,536 DWA values on this CPU), under a temporary waiver.

**Decided (owner, 2026-11-07): A.** Each path upstream takes is reproduced exactly, in safe code
(emulated lane by lane), and chosen as upstream chooses it; the emulations are checked against
the CPU's own instructions (`openexr-testkit/src/cpu.rs`, O9). Real SIMD kernels, for speed,
come after profiling: each proven equal to its emulation and added to the `unsafe` allowlist
by review.

**Recommendation: A, in the order of B.** Every codec lands scalar-exact first (which is also
the Windows wheel's DWA behaviour), and its SIMD paths land in the same phase, before the
codec counts as done. That keeps D12 without blocking the scalar port. NEON waits for ARM (D11).

### O4 — Crate layout and naming

**Recommendation:** the layout of §9: packages `openexr-rs-core`, `openexr-rs-deflate`,
`openexr-rs-zstd`, `openexr-rs-openjph` (free on crates.io today; `openexr`, `openexr-core` and
`exr` are taken), library names `openexr_core` and so on, directories `crates/openexr-core/`
and so on, nothing published. `openexr-rs` is kept for the later C++ `Imf` port, the one most
users would depend on.

### O5 — Minimum Rust version

**Recommendation: Rust 1.99.1**, pinned in `rust-toolchain.toml`, edition 2024,
`rust-version = "1.99.1"`. It is recon's toolchain and a rez package here (`rust-1.99.1`), so
the consumer builds the port with what it already uses. (OpenColorIO-rs is on 1.98.1.)

### O6 — DWA patents

DWAA and DWAB implement US 8,340,442 B1 ("Lossy compression of high-dynamic range image
files", filed 2010-02-12, expected expiry 2031-01-24) and US 8,666,186 B1 (its video
continuation, expected expiry 2030-02-12), both by Karl Richard Rasche, Pacific Data Images,
assigned to DreamWorks Animation LLC on 2024-12-16, both shown active by Google Patents. The
image patent claims decoding as well as encoding. Upstream's `PATENTS` grants a free patent
license for "this implementation of DreamWorks Lossy Compression within the OpenEXR standard";
it names only the C++ files, not Core's C translation (`internal_dwa*.c`).

**Decided (owner, 2026-11-07): credit DreamWorks.** The port keeps DreamWorks Animation in the
copyright line of every file upstream credits it in (`internal_dwa.c`, `internal_dwa_table.c`,
`internal_dwa_table_init.c`, `internal_b44_table.c`, `internal_b44_table_init.c`); `NOTICE`
names the patents and the grant; `THIRD-PARTY-NOTICES.md` carries `PATENTS` verbatim from the
DWA chunk on. DWA is used only inside EXR files, as the grant requires. Not chosen for now:
asking the OpenEXR project or DreamWorks to confirm in writing that the grant covers a
translation, and putting DWA behind a Cargo feature.

### O7 — OS error text

Core's default file streams put the system's error text into their messages:
`FormatMessage` with `FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS` and
`MAKELANGID (LANG_NEUTRAL, SUBLANG_DEFAULT)`, ANSI, its trailing line break kept
(`internal_win32_file_impl.h`), and `strerror` (`internal_posix_file_impl.h`). Rust's
`io::Error` text differs on Windows (wide API, another language id, the line break dropped).

**Decided (owner, 2026-11-07): exact.** One module, `crates/openexr-core/src/os_error.rs`, calls
the same C functions with the same arguments, on the `unsafe` allowlist. The wheel never reaches
this code (its C++ layer supplies its own streams), so it is checked against the C runtime and,
if O1 is approved, the source-built harness.

### O8 — What a Rust port can't have

**Decided (owner, 2026-11-07): not applicable, each with its reason in `upstream-map.toml` and
on the test:**
- user allocator hooks (`exr_set_default_memory_routines`, `memory.c`): Rust owns allocation and
  aborts on exhaustion; an allocation hook only if a consumer asks for one (§9);
- the out-of-memory test paths (`EXRCORE_TEST_RVAL_FAIL_MALLOC`): their tests are ported
  without those calls;
- `testInitializerCompat`: C structs versioned by size; the Rust initializer has one version;
- NULL-pointer calls (`exr_start_temporary_context (NULL, ...)`,
  `exr_print_context_info (NULL, ...)`, ...): a Rust reference is never NULL; the rest of each
  test is ported.

The ratchet counts ported tests plus listed n/a tests, out of 73.

### O9 — `unsafe` in the test kit

**Decided (owner, 2026-11-07): kept.** `crates/openexr-testkit/src/crt.rs` calls the platform's
`snprintf`, the reference for `printf` (CLAUDE.md rule 2); test-only, never a library
dependency, each call's format checked first. With it (owner, 2026-11-07),
`crates/openexr-testkit/src/cpu.rs` calls the CPU's instructions (`std::arch`), the reference
for the SIMD emulations (CLAUDE.md rule 2, "the CPU for instruction semantics"); each wrapper
checks the CPU has the instruction set and moves values through arrays of the instruction's
width.

---

## 15. Next steps

**Done (2026-11-06):**
- upstream pinned at `v3.5.3`; license and notice;
- upstream OpenEXRCore and OpenEXRCoreTest built locally, 73/73 on Windows (Release and Debug)
  and Rocky Linux 9.8 (Release) — `docs/baseline.md`;
- the oracle (`openexr==3.5.3`) working on both platforms, with the cross-platform comparison;
- this plan, `CLAUDE.md`, and `upstream-map.toml` (324 entries).

**Now (2026-11-07):** the lean Phase 0, then Phase 1 (§10), fast tier only (§8). No CI, no
pushing, no profiling until the owner asks.

**Done (2026-11-08): M3**, OpenEXRCore 3.5.3 parity (`docs/m3.md`), with the SIMD paths and
the speed work (`docs/baseline.md`, "Speed").

**Now (2026-11-09):** M4 (§16); O10-O12 decided.

---

## 16. M4: a safe Rust API (plan, 2026-11-09)

The milestone after M3 (O10, option C): a crate an ordinary Rust program can read and write
OpenEXR files with, safely, on top of the Core port. Not a port: a design, whose every pixel
and byte still comes from the verified Core port.

**The principle (owner, 2026-11-09):** the API is a wrapper over Core, as the C++ library and the
Python binding are, so it is not bound to copy their limits: "let's use the best we can". Below
it everything stays exact; above it, the best Rust design: lossless typed headers (every Core
attribute type kept distinct, where the Python binding folds float into double and drops type
hints), threading done well (decoding into the caller's buffers in parallel, reads overlapped
with decompression, deterministic errors, the application's own `rayon` pool), zero-copy borrows,
iterators, `Read + Seek` streams, builders, `Result` everywhere; with the C++ library's names for
its main types (O11).

### Definition of done

- Every file the M3 decode set holds (upstream's test images, `write_images.py`'s layouts, the
  corpus, the damaged copies) reads through the API to the wheel's pixels, bit for bit, and
  fails where the wheel fails, with Core's message.
- Every layout the wheel writes, written through the API from the wheel's pixels and header,
  gives the wheel's file, byte for byte.
- No `unsafe` in the crate (`#![forbid(unsafe_code)]`); every buffer access the API makes is
  checked once, up front, as Core's port checks its pipelines'.
- Both reference platforms, the fast and heavy tiers, as for Core.

### The API (sketch; O11)

Names follow the C++ library's, in Rust's case; behaviour follows Core's.

| Type | What it is |
|---|---|
| `Error` | Core's error code and message, as `Result<T, Error>` |
| `Header` | A part's header: the standard attributes as accessors (`data_window`, `display_window`, `channels`, `compression` and its levels, `line_order`, `tiles`, `name`, `part_type`), the rest as `(name, Attribute)`; `Attribute` an enum of Core's attribute types |
| `MultiPartInputFile`, `InputFile` | Open a path, a `Read + Seek` or bytes; the parts and their headers; `InputFile` is part 0 |
| `InputPart` | `read_pixels(&mut FrameBuffer, y_range)`, `read_tile(s)(&mut FrameBuffer, tiles, level)`, the levels and tile counts |
| `FrameBuffer<'a>`, `Slice<'a>` | The caller's buffers by channel name: a `Slice` is a pixel type, a `&'a mut` slice, the window it covers, strides, sampling and a fill value for channels the file lacks, checked once against the part |
| `DeepFrameBuffer<'a>` | The sample counts' slice, then per channel the samples, in buffers the caller sizes from the counts (no pointer arrays) |
| `MultiPartOutputFile`, `OutputFile`, `OutputPart` | Create a file from headers; `write_pixels(&FrameBuffer, lines)`, `write_tile`, deep writes; `finish()` reports the last write's error (drop cannot) |
| `Image` | The convenience layer, as the wheel's `OpenEXR.File`: `Image::read` gives every part's header and channels as owned planes; `Image::write` writes them |
| `ReadOptions`, `WriteOptions` | The thread count (O12), the strict-header and silent-header flags Core takes |

Halves are `u16` bit patterns (or `f32` through Core's exact conversion); an optional `half`
newtype comes later if wanted, without a dependency.

### How it is checked

- **The wheel's `OpenEXR.File`** (and `dump_pixels.py`'s output, which M3 already uses):
  `Image::read` and `FrameBuffer` reads of the M3 decode set give the wheel's pixels; refusals as
  the wheel's. Partial reads (line ranges, tiles, levels), other output types and sampled
  channels compare with the matching parts of the wheel's arrays.
- **Writing:** the wheel's pixels and headers written through the API give the wheel's files.
- **Headers:** `oracle/dump_headers.py` dumps the wheel's view of every header (one line per
  attribute, tab-separated); the API's `Header` must agree on every attribute's value, in the
  binding's view of it (`crates/openexr/tests/heavy_headers.rs`). The types the binding folds
  (float and double; a channel's pixel type, which only the binding's pixel reads carry) are
  checked against Core itself, whose names and types `openexr-core`'s heavy_harness checks
  against `core_harness info --verbose`.
- **Threaded reads and writes** give the single-threaded bytes and pixels (both from the wheel).
- **The API's own checks** (a stride that overruns its slice, a slice smaller than its window,
  an unknown channel, the wrong part type): unit tests, and refusals before Core is called.
- **Doc examples** compile and run (doctests).

### Phases

| Phase | Scope | Exit |
|---|---|---|
| **M4.0 Crate and headers** (~1 pw) | The crate, `Error`, `Header` and `Attribute`, `MultiPartInputFile` opening | Every header of the decode set as the wheel's. **Done 2026-11-09:** 1585 files, 20264 attributes; 11 refused by both, none by the wheel alone |
| **M4.1 Reading** (~2 pw) | `FrameBuffer`, `Slice`, scanline and tiled reads, levels, conversion, sampling, fill; `Image::read` | The decode set's pixels as the wheel's, whole and partial. **2026-11-09:** `FrameBuffer`, `Slice`, `InputPart` (`read_pixels`, `read_tiles`, levels): every flat part of the decode set as the wheel reads it (878 parts, 5682 reads: whole, interleaved, line ranges, sampled); refusals as the wheel's. Every level and every output type as Core's harness gives them (5985 level and type reads, 3303 above level 0). Left: `Image::read` (with M4.2) |
| **M4.2 Deep reading** (~1 pw) | `DeepFrameBuffer`, sample counts then samples | The deep files as the wheel's |
| **M4.3 Writing** (~2.5 pw) | `OutputFile`, `MultiPartOutputFile`, `OutputPart`, flat and deep, every compression and level; `Image::write` | The wheel's files, byte for byte |
| **M4.4 Threads, docs, speed** (~1.5 pw) | `ReadOptions`/`WriteOptions` threading, the crate's docs and examples, a speed comparison with Core's harness and with the C++ library's own multithreaded reads | Threaded results as single-threaded; docs build; the speed recorded |

About 8 person-weeks (range 6-10); at this project's pace, one to two weeks of calendar.

### Risks

- **The API's design** is the commitment: kept unpublished until the owner has reviewed it (O13).
- **The wheel's high-level API** covers whole-part reads; partial and strided reads are checked
  against slices of its arrays, which is exact but does not exercise the C++ library's own
  `FrameBuffer` code (that is M5's harness).

