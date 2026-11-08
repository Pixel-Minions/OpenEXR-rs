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
| `oracle/native_accept.py`: every layout of `oracle/write_images.py`, rewritten from its uncompressed copy into each of the 14 compressions by Core alone, against the wheel's own file of that compression, chunk by chunk | 2145 chunks byte-identical; 46 not, all from two upstream bugs of Core's writer, where the harness is not accepted: I-14 (the `attrs` layout's bytes attribute, which Core refuses to write) and I-23 (B44 and B44A on parts without half channels) | pending |
| `oracle/native_accept_messages.py`: the 53 images of upstream's test tree and the `write_images.py` set, each also cut, with 16 bytes of its first chunk set to 0xff, and with its first chunk's size set to 0x7fffffff: every Core message the wheel prints is printed by the harness, byte for byte | 1300 files, 627 with messages from the wheel, none missed | pending |
