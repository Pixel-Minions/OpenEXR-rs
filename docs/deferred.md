# Deferred work

Everything left for later, with the stage that picks it up. A stage starts by reading this
list and ends by striking (deleting) what it finished; nothing leaves the list unfinished
without the owner's OK.

| Item | Why it waits | Picked up by |
|---|---|---|
| The heavy tier: run `cargo xtask gate --full` and mark the chunks it covers done in `upstream-map.toml` | Owner, 2026-11-07: heavy tests run when everything is set | End of the port, or when the owner asks |
| Rocky Linux 9 run of the gate and of the oracle checks | Heavy tier | With the heavy tier |
| `docs/baseline.md`: confirm the Linux wheel's behaviour where it differs (DWA, the F16C half conversion) | Heavy tier | With the heavy tier |
| The source-built harness (PLAN.md O1) for Core's error and warning text and `exr_print_*` text, which the wheel can't show | Owner decision O1 open | When O1 is decided |
| The vendored libraries (PLAN.md O2): libdeflate, zstd, OpenJPH | Owner decision O2 open | Before 5a's ZIP, 5e, 5f |
| Decode: drop the per-store bounds checks of the unpack routines once a benchmark shows they cost (the up-front check of U-9 already covers every store) | Needs a benchmark | Phase 8, or earlier if decoding is slow |
| Tests of `unpack_16bit_{3,4}chan_planar`, `unpack_16bit_3chan`, `unpack_16bit` and `unpack_32bit` against `generic_unpack` | Only compressed chunks reach them | 5a (RLE) |
| `exr_chunk_default_initialize` (chunk.c:704-851) | Serves the codec tests in temporary contexts | Phase 5 |
| The codecs' decompressors and compressors (`decompress_data` answers `EXR_ERR_FEATURE_NOT_IMPLEMENTED` until then) | Phase 5 | 5a-5f |
| The deep unpackers (`generic_unpack_deep`, `generic_unpack_deep_pointers`); `internal_exr_match_decode` has none for deep parts yet | Phase 7 | 7 |
| Tests of D-3 (deep tile unpacked size message) and two D-6 messages | Need corrupt deep files | 7 |
| The F16C kernel of `half_to_float_buffer` (unpack.c:25-81), which GCC builds take at run time | SIMD | 8 |
| The write side: chunk writes (chunk.c:1676-2428), `write_header.c`, `encoding.c`, `pack.c`, the encode buffers of `coding.c`, `validation.c`'s write checks | Phase 6 | 6 |
| `exr_copy_unset_attributes`' upstream tests (write.cpp) | Phase 6 | 6 |
| The 2 GiB string and 134M-entry string vector checks of `general_attr.cpp` | Heavy tier | With the heavy tier |
| The decode heavy check (`tests/heavy_decode_none.rs`) for subsampled channels | Phase 4 compares unsampled parts only | 5a, with the codecs' check |
