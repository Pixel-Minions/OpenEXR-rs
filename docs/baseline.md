# Baseline: upstream OpenEXRCore and the oracle, 2026-11-06

What the pinned upstream does on this workstation, before any porting. Everything here was run
locally. There is no CI yet (owner, 2026-11-06).

## Machine and tools

| | |
|---|---|
| CPU | AMD Ryzen 9 9950X3D (Zen 5: SSE2 through AVX-512, F16C) |
| Windows | Windows 11 Pro 10.0.26300; MSVC 19.44.35229 (rez `msvc-14.44.35207`), Windows SDK 10.0.26100.0 |
| Linux | Rocky Linux 9.8 (glibc 2.34) in Docker on WSL2 (`docker/rocky9`); GCC 11.5.0 |
| Build tools | cmake 4.4.4 and ninja 1.13.0 from PyPI through `uvx`; uv 0.11.7 |
| Upstream | `upstream/openexr` at `v3.5.3`, 3950290530c4fd4776667d0dc4d65a9d003eb936 |
| Imath | Not vendored: upstream's CMake fetches it with `FetchContent` (`OPENEXR_IMATH_TAG`, `cmake/OpenEXRSetup.cmake:336-361`), `v3.2.3` = 5f27ba266d3ea1565e912570c30b5eafc89959f1. OpenEXRCore includes its `half.h` for half conversion (`internal_coding.h:15-24`) and `ImathConfig.h` (`openexr_config.h:12`); the C++ library and OpenEXRCoreTest link it |
| Vendored | libdeflate v1.26, zstd v1.5.7, OpenJPH 0.32.0 (`external/`) |

## Upstream build

`scripts/upstream-build.cmd` (Windows, inside the MSVC rez environment) and
`scripts/upstream-build.sh` (Linux, through `scripts/rocky9.sh`) configure upstream out of tree
under `target/`, with the dependency settings of the official wheel (upstream `pyproject.toml`,
`[tool.scikit-build.cmake.define]`): `OPENEXR_FORCE_INTERNAL_{DEFLATE,IMATH,OPENJPH,ZSTD}=ON`,
`BUILD_SHARED_LIBS=OFF`, position-independent code. They build `OpenEXRCoreTest`, which links
OpenEXRCore, the C++ OpenEXR library and Imath, and run the `OpenEXRCore` ctest label.

```
avr --non-interactive --run-as-tool --no-validation -p rez run rez-env msvc-14.44.35207 -- cmd /c scripts\upstream-build.cmd Release -j 8
scripts/rocky9.sh scripts/upstream-build.sh Release -j 8
```

| Platform | Configuration | Result | Wall time |
|---|---|---|---|
| Windows, MSVC 19.44 | Release (`/O2 /Ob2`) | **73/73 passed** | 4.6 s |
| Windows, MSVC 19.44 | Debug | **73/73 passed** | 17.3 s |
| Rocky Linux 9.8, GCC 11.5 | Release | **73/73 passed** | 3.8 s |

The 73 tests are the `define_openexrcore_tests` list in
`src/test/OpenEXRCoreTest/CMakeLists.txt` (10 base, 10 attribute, 3 color metadata, 15 read,
14 write, 21 compression), the same as the `TEST` entries of `main.cpp`. `CorePerfTest` is
built by the same CMake file but isn't a test; it belongs to the profiling step, later.

**Build difference that matters for bits.** CMake's probe `OPENEXR_IMF_HAVE_GCC_INLINE_ASM_AVX`
fails under MSVC and succeeds under GCC. Without it, `check_for_x86_simd`
(`internal_cpuid.h:93-113`) sets the `xgetbv` result to 0, so on Windows AVX and F16C always
read as absent: the DWA AVX and F16C paths (`internal_dwa_simd.h`) never run there. On Linux
they are chosen at run time from CPUID.

## The oracle

`oracle/` is a uv project pinned to `openexr==3.5.3` and `numpy==2.3.5` on Python 3.13
(`oracle/uv.lock`), like OpenColorIO-rs's. `oracle/smoke.py` writes three channels (every half
bit pattern, and random float and uint bit patterns from a fixed seed) with each of the 14
compressions, in a scanline and a tiled part, reads them back and compares the bits. It also
writes each file twice and records each file's SHA-256 and the SHA-256 of every decoded channel,
and opens the 41 `.exr` files of `src/test/OpenEXRTest` and `src/wrappers/python/tests`.

```
uv run --project oracle python -I oracle/smoke.py target/oracle-smoke/windows upstream/openexr/src/test/OpenEXRTest/*.exr upstream/openexr/src/wrappers/python/tests/*.exr
scripts/rocky9.sh uv run --project oracle --locked python -I oracle/smoke.py target/rocky9/oracle-smoke ...
```

Results, identical on both platforms:
- **28/28 cases pass.** Every pixel type comes back bit for bit through every codec that is
  lossless for it: NONE, RLE, ZIPS, ZIP, PIZ, ZSTD, HTJ2K256 and HTJ2K32 for all three types;
  PXR24 for half and uint; B44 and B44A for float and uint; DWAA and DWAB for uint. Writing the
  same pixels twice gives the same bytes, for every codec.
- Lossy channels, reported rather than checked: PXR24 changes 65,278 of 65,536 random floats;
  B44 and B44A change 49,664 of the 65,536 half patterns; DWAA and DWAB change 59,072 of them
  (`R`) and keep the float channel `Z` exactly, since the DWA classifier stores channels it
  doesn't name losslessly. LJ2K at its default quality kept all three channels exactly on this
  input.
- Of the 41 test images, 32 open and 9 are refused with upstream's own errors (the deliberately
  broken `comp_bad_*`, `comp_early_eof_piz`, `comp_invalid_unknown`, `comp_short_decode_piz`,
  `invalid`, `tiled` and `unfinished_multipart`); the listing is identical on both platforms.

**Windows against Linux, the same machine.** All 28 written files are byte-identical between the
`win_amd64` and the `manylinux_2_28_x86_64` wheels, including libdeflate, zstd, DWA and HTJ2K
output. Decoded pixels are identical too, with one exception: **DWAA and DWAB decode 2 of the
65,536 half values of `R` differently** (pixel (124, 198) in both: `0x8000` on Windows,
`0x8001` on Linux; one bit). Both wheels read the same file. This is the run-time CPU dispatch
above: the Linux wheel takes the AVX/F16C path on this CPU, the Windows wheel the SSE2/scalar
one. The port must reproduce both (PLAN.md D12, decision O3).

## What the wheel reaches in OpenEXRCore

The extension exports only `PyInit_OpenEXR`; Core's C API isn't callable from Python. The
binding (`src/wrappers/python/PyOpenEXR.cpp`) drives the C++ `Imf` classes, and in 3.5 these
reach Core as follows (the `exr_*` calls of `src/lib/OpenEXR/*.cpp`):
- **Reading:** `MultiPartInputFile` opens a Core context (`ImfContext.cpp`: `exr_start_read`),
  so Core parses every header (`parse_header.c`), validates it and reads the chunk table
  (`chunk.c`). The scanline, tiled and deep readers decode through `exr_decoding_*` and
  `exr_read_*chunk*`, so `decoding.c`, `unpack.c`, `coding.c` and every decompressor run.
- **Writing:** the C++ writers write the header and chunk table themselves (`ImfHeader`,
  `ImfOutputFile`), but compress through Core. `ImfCompressor.cpp:39-73` opens a temporary
  Core context (`exr_start_temporary_context`), copies the header into it
  (`Context::addHeader`, Core's part and attribute API) and sets the codec levels
  (`exr_set_*_compression_level`); `ImfCompressor.cpp:190-218` then runs
  `exr_encoding_initialize`, `exr_encoding_update` and `exr_compress_chunk` on data the C++
  side has already packed. So every compressor runs, at every level, but `pack.c` and
  `exr_encoding_run` don't.
- **Not reached:** Core's own writer (`exr_start_write`, `write_header.c`; `ImfContext.cpp`
  has a write mode, but nothing in the C++ library uses it), packing (`pack.c`), Core's error
  codes and messages for API misuse, and `exr_print_*` (`debug.c`).

## The source-built Core harness (PLAN.md O1, owner 2026-11-07)

`oracle/native/core_harness.c`, built by `scripts/oracle-native-build.cmd` (Windows, in the MSVC
rez environment) or `scripts/oracle-native-build.sh` (Rocky Linux 9) from the pinned submodule
with the wheel's settings (`oracle/native/CMakeLists.txt`, as `scripts/upstream-build.*`), into
`target/oracle-native/`. It drives OpenEXRCore 3.5.3 through its public C API only:
`core_harness info` (`exr_start_read`, `exr_print_context_info`, and with `--read` every chunk
decoded, Core's messages on stderr through its default handler) and `core_harness recompress`
(a file rewritten through Core alone: its header by `write_header.c`, its chunks packed by
`pack.c` and compressed at the levels asked for). It is used only for what the wheel can't
reach, and only where it reproduces the wheel:

| Check | Windows (2026-11-07) | Rocky Linux 9 |
|---|---|---|
| `oracle/native_accept.py`: every layout of `oracle/write_images.py`, rewritten from its uncompressed copy into each of the 14 compressions by Core alone, against the wheel's own file of that compression, chunk by chunk | 2145 chunks byte-identical; 46 not, all from two upstream bugs of Core's writer, where the harness is not accepted: I-14 (the `attrs` layout's bytes attribute, which Core refuses to write) and I-23 (B44 and B44A on parts without half channels) | 2026-11-08: the same, 2145 and 46 |
| `oracle/native_accept_messages.py`: the 53 images of upstream's test tree and the `write_images.py` set, each also cut, with 16 bytes of its first chunk set to 0xff, and with its first chunk's size set to 0x7fffffff: every Core message the wheel prints is printed by the harness, byte for byte | 1300 files, 627 with messages from the wheel, none missed | 2026-11-08: 1292 files, 623 with messages, none missed |

The port is checked against it by `crates/openexr-core/tests/heavy_harness.rs` (heavy tier,
Windows, 2026-11-07): every image of upstream's test tree and of `write_images.py`, whole and
damaged as above (1278 files), opened and read chunk by chunk as `core_harness info --read`
does, per-chunk results, a digest (FNV-1a 64) of every decoded chunk's bytes, and Core's and
OpenJPH's messages, line for line, with header parsing silent and not; and
`exr_print_context_info`'s verbose text, byte for byte: identical but for the lines of the
waived deviations D-4 (178) and D-6 (5). Every `write_images.py` layout, the multipart files
of `oracle/write_multipart.py`, and upstream's images that decode cleanly (multipart.0001.exr,
mipmapped tiles, real pictures among them) rewritten by Core alone into each compression,
ZIP and ZIPS at levels 0-9, ZSTD at -1 to 22, DWAA and DWAB at five levels: the header byte
for byte and every chunk by index, 2909 rewrites identical; left out, D-10, D-17 and D-18
(124), and the ZSTD levels whose strategies are not yet ported (969). Upstream's pictures
found a port bug the wheel's images could not: libdeflate's `min_lens` table had two entries
wrong (fixed).

2026-11-08, with the corpus (`scripts/fetch-corpus.sh`: openexr-images at e38ffb07, 97
images) and every ZSTD level ported, Windows: 1612 files read and printed (the corpus whole
and damaged among them), identical but for D-4 (178) and D-6 (9); 5068 rewrites (the
corpus's at each compression's default level), identical, with the chunks of D-10, D-17 and
D-18 left out (3367, nearly all D-17's mip-level edge tiles of the corpus's tiled files). And
`core_harness convert`: each of the six conversions between half, float and uint, over every
input (2^16 halfs, 2^32 floats and uints), through Core's pack and unpack routines, as
digests per 2^24 inputs: the port's 2052 digests equal Core's, and its three one-line files
equal Core's byte for byte; on Rocky Linux 9 too (its harness unpacks halves to floats with
F16C on this CPU).

zstd frames past `ZSTD_CURRENT_MAX` (2026-11-08, Windows): `core_harness zstd-big`, a 3.76 GB
frame at levels 1, 3, 5, 9, 13, 16, 19 and 22, and 1 GB frames continuing four at a time at
levels 1, 9, 13, 16 and 22 (`tests/heavy_zstd_big.rs`): every digest equal to Core's, level 22
with long distance matching; both checks 227 s; peak memory 14.7 GB (Core) and 18.2 GB (port).

## Rocky Linux 9 (2026-11-08)

`scripts/rocky9.sh` runs the reference container (`docker/rocky9`, image `openexr-rs-rocky9:2`:
Rocky Linux 9.8, GCC 11.5, uv, and rust-toolchain.toml's Rust 1.99.1), the checkout at `/work`,
Linux build output (cargo's included) in a volume at `/work/target/rocky9`. On this machine (AMD
Ryzen 9 9950X3D: AVX-512F, AVX2, F16C, BMI1 and BMI2) `scripts/rocky9.sh cargo xtask gate --full`
passes: the fast tier and the heavy one, the wheel's decode and encode checks (DWA's AVX inverse
DCT and F16C conversions, the F16C half kernel and zstd's assembly Huffman loop among the paths
taken) and the comparison with the Linux harness.

The run found one difference of the platforms, now ported: the Linux wheel's OpenJPH builds its
AVX-512 irreversible lifting with GCC in `-std=gnu++14`, which contracts `p + f * (m + n)` and
`p - f * (m + n)` into fused multiply-adds; the wheel's whole module holds exactly those five
(`vfmadd213ps` three times, `vfnmadd213ps` twice), and the port computes them fused on GCC-built
x86-64 at OpenJPH's AVX-512 level (`crates/openexr-openjph/src/transform.rs`). MSVC does not
contract, so the Windows wheel lifts unfused.

A second difference of the platforms, ported: for an ATK-Eatk past a sample's width, the Linux
wheel's GCC-vectorized general lifting loops shift differently from the Windows wheel's scalar
ones (`docs/improvements.md`, I-33).

## Speed (2026-11-08, Windows)

`scripts/bench.py`: Core (`core_harness bench`, the wheel's build settings, MSVC `/O2`) and the
port (`cargo run --release -p openexr-rs-core --example bench`, the same work through the port's
API), single-threaded, on three corpus images (Blobbies: scanlines, 5 channels; Ocean: tiles;
Beachball/singlepart.0001: 20 channels; about 1 MP each) rewritten by Core into each
compression: every chunk decoded into planar buffers (the file's reads included), and every
chunk encoded with a `write_fn` that writes nothing; best of 7 runs over 3 rounds, summed over
the images. The port's time over Core's, before the speed work and after each round
(profiles: `perf` on Rocky Linux 9, of the port and, side by side, of Core's harness):

| Compression | Decode, before | round 1 | round 2 | round 3 | Encode, before | round 1 | round 2 | round 3 |
|---|---|---|---|---|---|---|---|---|
| NONE | 1.02 | 1.02 | 1.02 | 1.02 | 5.00 | 0.43 | 0.44 | 0.41 |
| RLE | 0.45 | 0.46 | 0.43 | 0.39 | 1.20 | 0.93 | 0.88 | 1.00 |
| ZIPS | 1.62 | 1.35 | 1.28 | 1.12 | 1.66 | 1.30 | 1.17 | 1.23 |
| ZIP | 1.58 | 1.29 | 1.04 | 0.87 | 1.58 | 1.23 | 1.09 | 1.02 |
| PIZ | 1.03 | 1.04 | 1.06 | 0.95 | 0.95 | 0.86 | 0.86 | 0.83 |
| PXR24 | 1.43 | 1.19 | 1.15 | 0.94 | 1.66 | 1.28 | 1.13 | 1.07 |
| B44 | 0.80 | 0.80 | 0.74 | 0.45 | 1.15 | 0.84 | 0.85 | 0.73 |
| B44A | 0.80 | 0.82 | 0.76 | 0.43 | 1.12 | 0.81 | 0.82 | 0.69 |
| DWAA | 1.31 | 1.32 | 1.29 | 0.82 | 1.17 | 1.14 | 1.05 | 0.90 |
| DWAB | 1.18 | 1.15 | 1.13 | 0.83 | 1.18 | 1.10 | 1.04 | 0.90 |
| HTJ2K256 | 1.55 | 1.56 | 1.53 | 1.47 | 1.46 | 1.31 | 1.29 | 1.27 |
| HTJ2K32 | 1.34 | 1.30 | 1.31 | 1.30 | 1.36 | 1.28 | 1.26 | 1.25 |
| LJ2K | 1.73 | 1.79 | 1.75 | 1.70 | 1.81 | 1.44 | 1.42 | 1.40 |
| ZSTD | 1.44 | 1.07 | 1.05 | 0.93 | 1.50 | 1.18 | 1.17 | 1.25 |

The changes, none of which changes a byte: packing takes a line whose every read and store is in
bounds through a loop specialized to its types (a copy for the same type), where the per-sample
checks cannot fail; ZIP's predictor and interleave work as upstream's SSE2 versions do, in
eight-byte words; libdeflate's hash-chain matchfinder masks its table indices (a no-op) so that
they need no bounds checks, and its decoder copies match words without `memmove`; zstd's row
matchfinder compares tags as upstream's SWAR branch does; Core's ZSTD delta and byte shuffles
run without per-element checks; Adler-32 sums sixteen lanes; LJ2K's float rounding needs no
library call. Round 2: unpacking takes a line whose every store is in bounds through a loop
specialized to its types, as packing does; libdeflate's matchfinder and `lz_extend` are inlined
where upstream's are `forceinline` (Core's whole lazy compressor is one function), the lazy
compressor is compiled for each mode, and the matchfinder's bucket prefetches are restored as
plain loads. Round 3: the codecs' and pipelines' buffers come from a per-thread pool
(`buffer_pool.rs`), zeroed to the length asked as a new one is, instead of fresh pages for every
chunk; ZIP's predictor waits on one byte add per word; Adler-32 keeps per-lane running sums, so it
vectorizes. What remains is mostly where Core
uses SIMD the port does not (OpenJPH's block coders, libdeflate's decoder, DWA) and zeroed
buffers' zeroing where upstream leaves them uninitialized.

### After the SIMD kernels (2026-11-08, Windows)

With every SIMD path ported (OpenJPH's block coders, wavelets and conversions; ZIP's, DWA's,
the ZSTD codec's and libdeflate's kernels; zstd's BMI2 builds on GCC) and the compressed buffer
taken unzeroed, `scripts/bench.py 7 3` on a quiet machine, the port's time over Core's:

| Compression | Decode | Encode |
|---|---|---|
| NONE | 1.02 | 0.41 |
| RLE | 0.40 | 0.67 |
| ZIPS | 0.94 | 1.00 |
| ZIP | 0.82 | 0.97 |
| PIZ | 0.91 | 0.85 |
| PXR24 | 0.86 | 1.01 |
| B44 | 0.46 | 0.70 |
| B44A | 0.44 | 0.67 |
| DWAA | 0.79 | 0.87 |
| DWAB | 0.81 | 0.89 |
| HTJ2K256 | 1.06 | 0.88 |
| HTJ2K32 | 0.90 | 0.97 |
| LJ2K | 1.03 | 0.94 |
| ZSTD | 0.82 | 1.05 |

(ZSTD encoding on Linux stays about 1.2 to 1.35 times Core's: `docs/deferred.md`.)

## Emulated CPUs (2026-11-08)

`scripts/sde.sh <cpu>` with SDE 10.13.1, `tests/heavy_decode.rs` (the wheel's decode, header
and encode checks, with the crafted ATK kernels; under SDE the `write_images` layouts), the
port and the wheel on the same emulated CPU, at `28ff921`: all four checks pass at nhm, snb,
ivb, hsw and skx, on Windows and on Rocky Linux 9 (ten runs, 33 to 55 minutes each). One
Windows ivb run first failed before the oracle ran: SDE could not attach to the wheel's Python
process (its log: `WIN_FOLLOW_CHILD: Failed to inject`), with a concurrent big-frame check using
most of the memory; alone, and on rerun, it passes.

### Buffers reused unzeroed (2026-11-09, Windows)

The decode and encode pipelines' buffers, and DWA's planar buffers, are taken from the
per-thread pool with their contents unspecified, as upstream's `alloc_fn` leaves them (only
DWA's AC, DC and RLE buffers stay zeroed, as upstream memsets them); heavy-tier builds fill them
with a pattern (`buffer_pool::reused`), and `gate --full` passes on Windows and Rocky Linux 9
(308 tests each). `scripts/bench.py 7 3`, the port's time over Core's:

| Compression | Decode | Encode |
|---|---|---|
| NONE | 1.02 | 0.38 |
| RLE | 0.38 | 0.74 |
| ZIPS | 0.91 | 0.99 |
| ZIP | 0.80 | 0.99 |
| PIZ | 0.89 | 0.85 |
| PXR24 | 0.82 | 1.03 |
| B44 | 0.42 | 0.69 |
| B44A | 0.41 | 0.66 |
| DWAA | 0.78 | 0.84 |
| DWAB | 0.79 | 0.86 |
| HTJ2K256 | 1.03 | 0.88 |
| HTJ2K32 | 0.96 | 0.94 |
| LJ2K | 1.00 | 0.91 |
| ZSTD | 0.64 | 1.03 |

## The safe API's speed (2026-11-09, Windows; remeasured after the thread pools)

`scripts/bench_api.py 5`: the files of `scripts/bench.py` (Blobbies, scanlines; Ocean, tiles;
singlepart.0001, 20 channels; each rewritten by Core into all 14 compressions), each read whole
and written whole, by the wheel (the C++ library: `OpenEXR.File(..., num_threads=n)` and
`File.write`) and by the API (`Image::read` from memory, `Image::write_to` into memory), the
best of 5, milliseconds summed over the three images. One thread is the calling thread alone
on both sides (the C++ library's `num_threads=1` is a worker beside it, which overlaps
compression with the rest: its writes then take about half as long); the right half is 32
threads (the C++ library's pool set to 32 with `set_global_thread_count`). The wheel's times
include building its numpy arrays and writing to a file; the API's, its owned planes and
writing into memory. This machine: AMD Ryzen 9 9950X3D, 16 cores, 32 threads.

One thread on the left, 32 on the right:

| Compression | Read C++ | Read API | Write C++ | Write API | Read C++ | Read API | Write C++ | Write API |
|---|---|---|---|---|---|---|---|---|
| NONE | 59.4 | 8.9 | 52.4 | 25.2 | 152.9 | 9.2 | 28.9 | 30.5 |
| RLE | 117.2 | 40.8 | 118.4 | 74.0 | 26.4 | 11.2 | 32.8 | 48.9 |
| ZIPS | 48.8 | 42.9 | 231.9 | 202.2 | 27.5 | 11.6 | 50.0 | 47.6 |
| ZIP | 40.2 | 37.5 | 203.7 | 208.2 | 11.4 | 16.7 | 28.9 | 49.3 |
| PIZ | 95.8 | 96.5 | 279.4 | 273.6 | 15.8 | 26.7 | 40.6 | 73.6 |
| PXR24 | 44.6 | 44.4 | 181.1 | 196.9 | 11.4 | 23.8 | 25.2 | 48.9 |
| B44 | 29.0 | 27.2 | 95.8 | 91.3 | 13.2 | 15.0 | 23.5 | 42.7 |
| B44A | 25.6 | 23.3 | 100.5 | 83.8 | 12.3 | 15.0 | 21.7 | 42.1 |
| DWAA | 74.2 | 63.4 | 707.2 | 722.4 | 16.0 | 26.4 | 129.9 | 143.6 |
| DWAB | 70.9 | 64.7 | 451.4 | 470.7 | 22.6 | 26.3 | 180.9 | 185.9 |
| HTJ2K256 | 82.9 | 83.6 | 176.6 | 150.2 | 37.4 | 33.4 | 60.9 | 62.7 |
| HTJ2K32 | 75.7 | 91.0 | 205.7 | 162.9 | 42.3 | 33.1 | 40.5 | 53.2 |
| LJ2K | 83.9 | 96.3 | 169.4 | 164.8 | 42.0 | 37.1 | 61.5 | 66.1 |
| ZSTD | 67.7 | 44.5 | 246.7 | 205.2 | 25.1 | 11.6 | 26.6 | 51.5 |

- **One thread:** the API reads as fast as the C++ library or faster, but for HTJ2K32 (1.20
  times the C++ library's time) and LJ2K (1.15), and writes as fast or faster, but for DWAA,
  DWAB, PXR24 and ZIP (1.02 to 1.09). OpenJPH's line, codeblock and coded-byte buffers come
  from a pool each thread keeps (`crates/openexr-openjph/src/mem.rs`), as upstream carves them
  from allocators it keeps from one codestream to the next; that took HTJ2K32's read from 1.83
  times the C++ library's time to 1.20, and its write from 1.18 to 0.79.
- **32 threads:** reads decode their chunks in parallel straight into the caller's buffers, and
  writes compress theirs in parallel. A `Threads::Count(n)` pool is the process's, kept from
  file to file as the C++ library keeps its global pool (it was the file's own, its threads
  and their buffers started anew for each file, which made HTJ2K32 slower on 32 threads than on
  4: 265 ms to read, now 33). Reads are faster than the C++ library's for NONE, RLE, ZIPS,
  ZSTD and the HTJ2K family, slower for the rest (up to 2.1 times its time, PXR24); writes
  scale less (the C++ library overlaps staging, compression and writing;
  the API stages and writes on the calling thread: `docs/deferred.md`).

