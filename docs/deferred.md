# Deferred work

Everything left for later, with the stage that picks it up. A stage starts by reading this
list and ends by striking (deleting) what it finished; nothing leaves the list unfinished
without the owner's OK.

| Item | Why it waits | Picked up by |
|---|---|---|
| The heavy tier: run `cargo xtask gate --full` and mark the chunks it covers done in `upstream-map.toml` | Owner, 2026-11-07: heavy tests run when everything is set | End of the port, or when the owner asks |
| Rocky Linux 9 run of the gate and of the oracle checks | Heavy tier | With the heavy tier |
| `docs/baseline.md`: confirm the Linux wheel's behaviour where it differs (DWA: the AVX inverse DCT and the F16C conversions, emulated in `internal_dwa_simd.rs`, which only a Linux run against the wheel checks; the half conversion), and B44's and DWA's tables, which glibc's expf, logf and powf build there (checked on Windows only) | Heavy tier | With the heavy tier |
| The source-built harness (PLAN.md O1) for Core's error and warning text and `exr_print_*` text, which the wheel can't show | Owner decision O1 open | When O1 is decided |
| The vendored libraries (PLAN.md O2, decided A: each ported exactly as its own crate): libdeflate's decompressor and its compressor's levels 0 to 9 are in; its other levels, zstd and OpenJPH remain | Ported with the codecs that use them | 6 (libdeflate's other levels), 5e (zstd), 5f (OpenJPH) |
| Decode: drop the per-store bounds checks of the unpack routines once a benchmark shows they cost (the up-front check of U-9 already covers every store) | Needs a benchmark | Phase 8, or earlier if decoding is slow |
| The codecs' decompressors and compressors (`decompress_data` and `exr_compress_chunk` answer `EXR_ERR_FEATURE_NOT_IMPLEMENTED` until then); NONE, RLE, ZIP, ZIPS, PIZ, PXR24, B44, B44A, DWAA and DWAB are in both ways | Phase 5 | 5b-5f |
| The deep unpackers (`generic_unpack_deep`, `generic_unpack_deep_pointers`); `internal_exr_match_decode` has none for deep parts yet | Phase 7 | 7 |
| Tests of D-3 (deep tile unpacked size message) and two D-6 messages | Need corrupt deep files | 7 |
| The F16C kernel of `half_to_float_buffer` (unpack.c:25-81), which GCC builds take at run time | SIMD | 8 |
| The 2 GiB string and 134M-entry string vector checks of `general_attr.cpp` | Heavy tier | With the heavy tier |
| Compare Core's error text with the wheel's: the wheel prints Core's own messages on stderr (for example "(EXR_ERR_BAD_CHUNK_LEADER) Invalid chunk size reconstructing chunk table: found out of range 2013265920" for tiled.exr), so the heavy decode check can capture them and compare them with the port's | Found while adding ZIP (5a); a heavy-tier check | 5a's end, or with the heavy tier |
| libdeflate's near-optimal compression levels (10-12, `bt_matchfinder.h`), which only a direct `exr_compress_buffer` call reaches; until then `Compressor::new` gives none, and `exr_compress_buffer` answers `EXR_ERR_OUT_OF_MEMORY`, for them. The wheel writes the default level, 4 (its binding has no setting for another), so the streams of every other level (0-3 and 5-9 are in) can be checked byte for byte only by a source-built harness (PLAN.md O1) | Unreachable through Core's ZIP levels, and uncheckable until O1 (owner, 2026-11-07: parked) | End of the port, after O1 |
| Performance: `exr_compress_buffer` allocates a compressor (about 700 KB of tables) for every chunk, as upstream does; a compressor cached in the pipeline would give the same bytes | A benchmark | 8 |
| Performance kernels that give upstream's results by construction: ZIP's SSE2/SSE4.1 predictor and interleave, libdeflate's BMI2 decoder and vectorized Adler-32 | PLAN.md O3 (open) and a benchmark | 8 |
| `exr_compress_chunk`'s deep sample count table compression (`compression.c:558-622`), which upstream runs on the freshly allocated, unfilled table and never stores (so a compressed deep chunk written by Core alone is refused for its 0-byte table); an I- entry with the deep writers | The deep writers | 7 |

