# Improvement candidates

Upstream bugs and limitations that the port copies, so that its results match OpenEXR 3.5.3
byte for byte, but that could be fixed. The owner decides on each one: keep it, or fix it as a
deviation (`docs/deviations.md`, with its waiver in `docs/waivers.md`), which changes the
port's results for the inputs the entry names. The rules are OpenColorIO-rs's (`CLAUDE.md`,
rule 1). Entries the owner has decided to fix say **fixed** and name their deviation.

**Adding an entry.** A chunk that ports an upstream bug or limitation, or a platform
difference that exists only because of how the wheels were built, adds an entry here in the
same chunk. Numbers are never changed or reused: take the next free one in the series (`I-`
for behaviour copied, `U-` for undefined behaviour the port refuses instead), whatever the
section. An entry says:
- **Upstream:** what OpenEXR does, with its source (`file:line @ v3.5.3`) or the oracle probe
  that showed it;
- **Who notices:** the inputs or calls affected, in plain words;
- **A fix:** what the port would do instead, and what that changes;
- **Status:** matched (with the chunk), to be matched (with the phase), open, or fixed (a
  deviation the owner approved); and how it is verified.

## Base

### I-1. Two error codes have no name

- **Upstream:** `the_error_code_names` lists 34 names for the 36 codes of `exr_error_code_t`:
  `EXR_ERR_COMPRESSION_FAILED` (33) and `EXR_ERR_DECOMPRESSION_FAILED` (34) are missing
  (`src/lib/OpenEXRCore/base.c:29-63`, `openexr_errors.h:30-69` @ v3.5.3). Index 33 is
  `"EXR_ERR_UNKNOWN"`, and every index from 34 on falls back to the last entry, so
  `exr_get_error_code_as_string` returns `"EXR_ERR_UNKNOWN"` for both codes.
  `exr_get_default_error_message` is not affected: its table has all 36 messages.
- **Who notices:** callers that print the name of a failed compression or decompression.
- **A fix:** add the two names before `"EXR_ERR_UNKNOWN"`.
- **Status:** matched (`crates/openexr-core/src/base.rs`). Not verified against a running
  library: the wheel can't call Core's C API, so this waits for the source-built harness
  (PLAN.md O1, heavy tier).

## Pixel conversions

### I-2. The float 2^32 converts to the integer 0

- **Upstream:** `float_to_uint` returns `UINT32_MAX` for values above `(float) UINT32_MAX`,
  which is 2^32, so exactly 2^32 reaches `(uint32_t) fv`, undefined in C
  (`src/lib/OpenEXRCore/internal_coding.h:260-266` @ v3.5.3). MSVC 19.44 (`/O2`) and GCC 11.5
  (`-O2`, `-O3`, a loop as well as a single call) both convert through a 64-bit integer and keep
  the low 32 bits: 0 (a standalone probe of the same function, 2026-11-07).
- **Who notices:** a float channel read into a uint channel, at the one float value 2^32
  (`4294967296.0f`); every larger float already gives `UINT32_MAX`.
- **A fix:** compare with `>=`, so 2^32 gives `UINT32_MAX` like its neighbours.
- **Status:** matched (`crates/openexr-core/src/internal_coding.rs`). The probe compiled the
  function on its own; Core's unpackers inline it, so the heavy tier checks it again at their
  call sites when unpacking lands (Phase 4).

## Attributes

### I-3. The computed size of an attribute list is not its size in a file

- **Upstream:** `exr_attr_list_compute_size` counts a `bytes` attribute as
  `sizeof (exr_attr_bytes_t)` (32 on x86-64) plus its data, where a file holds a 4-byte hint
  length, the hint and the data; it counts no terminating NUL for a channel list and no width
  and height for a preview (`src/lib/OpenEXRCore/attributes.c:317-425` @ v3.5.3). Nothing in
  the library calls it (the header writer measures as it writes).
- **Who notices:** callers of the function itself (it is internal; upstream's tests call it).
- **A fix:** count what `write_header.c` writes.
- **Status:** matched (`crates/openexr-core/src/attributes.rs`).

## Header parsing

### I-4. A scanline part's chunk count is computed in int

- **Upstream:** `internal_exr_compute_chunk_offset_size` converts the data window height to
  `int` and computes `(h + linePerChunk - 1) / linePerChunk` in `int`
  (`src/lib/OpenEXRCore/parse_header.c:2576-2578` @ v3.5.3). Validation allows heights up to
  about 2^31 - 3, so with 256 lines per chunk (DWAB, HTJ2K256, LJ2K) the sum can pass
  `INT_MAX`, which is undefined in C; x86-64 wraps, giving a negative count, and the part is
  refused as "Invalid chunk count".
- **Who notices:** crafted files with data windows near 2^31 rows.
- **A fix:** compute in 64 bits.
- **Status:** matched (wrapping arithmetic, `crates/openexr-core/src/parse_header.rs`); the
  oracle check of such a file belongs to the heavy tier.

### I-5. A negative enum value passes the enum setters on Windows

- **Upstream:** `exr_set_lineorder` refuses `lo >= EXR_LINEORDER_LAST_TYPE`
  (`src/lib/OpenEXRCore/part_attr.c:813-821` @ v3.5.3), and `exr_attr_set_compression`,
  `exr_attr_set_envmap` and `exr_attr_set_lineorder` refuse `>= EXR_COMPRESSION_LAST_TYPE`,
  `EXR_ENVMAP_LAST_TYPE` and `EXR_LINEORDER_LAST_TYPE` (lines 1508-1516, 1560-1568 and
  1764-1772). An enum's underlying type is implementation-defined: `int` under MSVC,
  `unsigned int` under GCC (an enum with no negative enumerators). So a negative value passed
  in is refused on Linux and stored on Windows, as `(uint8_t) value`.
- **Who notices:** callers passing an invalid line order, compression or envmap through a cast.
- **A fix:** check `value < 0` too.
- **Status:** **fixed**, deviation D-1 (owner, 2026-11-07): a negative value is refused on every platform
  (`crates/openexr-core/src/part_attr.rs`, `enum_out_of_range`); checked by
  `part_attr_set::tests::negative_enum_values_are_refused`.

### I-6. The debug dump computes a box's size in int

- **Upstream:** `print_attr` prints a `box2i` as `max.x - min.x + 1` by `max.y - min.y + 1` in
  `int` (`src/lib/OpenEXRCore/debug.c:27-36` @ v3.5.3), which overflows (undefined) for boxes
  wider than `INT_MAX`; a data window set through `exr_set_data_window` is not checked first.
  The wheels' code wraps.
- **Who notices:** callers printing a context whose window spans more than 2^31 pixels.
- **A fix:** compute in 64 bits.
- **Status:** **fixed**, deviation D-2 (owner, 2026-11-07): computed and printed in 64 bits
  (`crates/openexr-core/src/debug.rs`); checked by `debug::tests::dump_prints_every_type`.

### I-9. exr_copy_unset_attributes fails on a deepImageState attribute

- **Upstream:** `copy_attr` copies each attribute's value by type, but its `switch` has no case
  for `EXR_ATTR_DEEP_IMAGE_STATE`, which falls to the default: `EXR_ERR_INVALID_ATTR`, the new
  attribute removed again (`src/lib/OpenEXRCore/part_attr.c:497` @ v3.5.3). The copy stops
  there, leaving the attributes copied before it.
- **Who notices:** callers copying the header of a deep part that has the standard
  `deepImageState` attribute.
- **A fix:** copy its byte, as for the other one-byte types.
- **Status:** **fixed**, deviation D-5 (owner, 2026-11-07) (`crates/openexr-core/src/part_attr_copy.rs`);
  checked by `part_attr_copy::tests::copies_deep_image_state`.

### I-11. exr_set_version refuses without a message

- **Upstream:** a version other than 1 returns `EXR_ERR_ARGUMENT_OUT_OF_RANGE` without calling
  the error handler (`src/lib/OpenEXRCore/part_attr.c:1114` @ v3.5.3), where every other
  refusal of the setters reports why.
- **Who notices:** callers setting a deep part's version through `exr_set_version` or
  `exr_attr_set_int (f, p, "version", v)`.
- **A fix:** report "Invalid version %d for deep data: only version 1 is supported".
- **Status:** **fixed**, deviation D-7 (owner, 2026-11-07) (`crates/openexr-core/src/part_attr_set.rs`);
  checked by `part_attr_set::tests::refusals_carry_messages`.

### I-12. exr_copy_unset_attributes counts a failed lookup as found

- **Upstream:** for each source attribute, a lookup in the destination that fails with
  anything but `EXR_ERR_NO_ATTR_BY_NAME` is taken as success, and the attribute is skipped
  (`src/lib/OpenEXRCore/part_attr.c:573` @ v3.5.3).
- **Who notices:** no caller today: the source's names are valid, so the lookup can only find
  the name or not.
- **A fix:** return the lookup's error.
- **Status:** **fixed**, deviation D-8 (owner, 2026-11-07) (`crates/openexr-core/src/part_attr_copy.rs`).

## Header writing

### I-14. Core writes a bytes attribute without its type hint

- **Upstream:** `save_bytes` writes a `bytes` attribute as its size and data
  (`src/lib/OpenEXRCore/write_header.c:139-167` @ v3.5.3), where the format, and both
  readers, hold the type hint's length, the hint and the data (`parse_header.c`'s
  `extract_attr_bytes`, `ImfBytesAttribute.cpp:101-113`): a file Core writes with a `bytes`
  attribute does not read back. It also refuses an empty attribute, whose data is NULL.
- **Who notices:** writers of `bytes` attributes through Core.
- **A fix:** write the hint length, the hint and the data, as the C++ library does; write an
  empty attribute.
- **Status:** **fixed**, deviation D-10 (owner, 2026-11-07) (`crates/openexr-core/src/write_header.rs`);
  checked by `write_header::tests::bytes_attributes_read_back` and, against the C++
  library's bytes, the heavy `headers_rewrite_as_the_wheel_wrote_them` (an attribute of
  every type the binding writes, `bytes` with and without a hint and empty).

### I-15. exr_write_header writes multipart files no reader opens

- **Upstream:** `exr_write_header` validates each part on its own but not the attributes the
  parts must share (displayWindow, pixelAspectRatio, timecode, chromaticities), which every
  reader checks; upstream's own TODO says so (`src/lib/OpenEXRCore/context.c:711-721`
  @ v3.5.3).
- **Who notices:** writers of multipart files whose parts disagree on one of them.
- **A fix:** refuse them as the reader does, before writing anything.
- **Status:** **fixed**, deviation D-11 (owner, 2026-11-07) (`crates/openexr-core/src/write_header.rs`, with the
  reader's message); checked by `write_header::tests::shared_attributes_are_checked_on_write`.

### I-16. Core cannot write a deepImageState attribute

- **Upstream:** `save_attr` has no case for `EXR_ATTR_DEEP_IMAGE_STATE`, which falls to the
  default, `EXR_ERR_INVALID_ATTR` (`src/lib/OpenEXRCore/write_header.c:571-626` @ v3.5.3), so a
  header holding the standard `deepImageState` attribute cannot be written.
- **Who notices:** writers of deep parts with that attribute.
- **A fix:** write its byte, as `DeepImageStateAttribute::writeValueTo` does
  (`ImfDeepImageStateAttribute.cpp:35-40`).
- **Status:** **fixed**, deviation D-12 (owner, 2026-11-07) (`crates/openexr-core/src/write_header.rs`); checked
  by `write_header::tests::deep_image_state_reads_back`.

### I-17. exr_write_scanline_chunk_info sizes a chunk from the line it was given

- **Upstream:** `exr_write_scanline_chunk_info` computes the chunk's unpacked size from `y`,
  the line it was asked about, rather than the chunk's first line, and tests the end of the
  data window with `miny + lpc` in `int` (`src/lib/OpenEXRCore/chunk.c:1870-1958` @ v3.5.3);
  the read side (`exr_read_scanline_chunk_info`) uses the first line and 64 bits.
- **Who notices:** writers asking about a line in the middle of a chunk of a part with
  y-subsampled channels (a wrong size); windows ending near `INT_MAX` (an overflow).
- **A fix:** compute as the read side does.
- **Status:** **fixed**, deviation D-13 (owner, 2026-11-07) (`crates/openexr-core/src/chunk_write.rs`).

### I-18. Core writes a decreasing-Y scanline file's chunks top down

- **Upstream:** the chunk writes accept only the next chunk index up, whatever the line
  order (`src/lib/OpenEXRCore/chunk.c:1779-1783`, `2415-2424` @ v3.5.3; the reversal is
  commented out), so a `DECREASING_Y` file stores its chunks from the top, against its line
  order, where the C++ library stores them from the bottom (`ImfOutputFile.cpp`).
- **Who notices:** readers that rely on the line order to read a file in storage order, and
  anyone comparing Core's files with the C++ library's.
- **A fix:** ask for the chunks in the C++ library's order: for scanline parts, from the last
  one down; for tiled parts, the levels in chunk order, each from its top row of tiles down,
  each row left to right (`TiledOutputFile::Data::nextTileCoord`,
  `src/lib/OpenEXR/ImfTiledOutputFile.cpp:281-386`, `995-997` @ v3.5.3).
- **Status:** **fixed**, deviation D-14 (owner, 2026-11-07) (`crates/openexr-core/src/
  chunk_write.rs`); checked by `chunk_write::tests::files_rewrite_as_the_cpp_library_wrote_them`
  on lineOrder_decreasing.exr, by the heavy tier on the wheel's decreasing scanline and
  (one-level) tiled files, and, for the order across levels, which the wheel cannot write, by
  `chunk_write::tests::decreasing_mipmapped_tiles_go_in_the_cpp_librarys_order`.
### I-20. An encode pipeline without a pack stage writes an empty chunk

- **Upstream:** `exr_encoding_run` zeroes `packed_bytes` before packing; with no
  `convert_and_pack_fn` it checks the caller's packed buffer but never sets `packed_bytes`
  again, so the compress stage is skipped and the chunk is written with 0 bytes of data
  (`src/lib/OpenEXRCore/encoding.c:293-315`, `331-361` @ v3.5.3).
- **Who notices:** callers who pack the chunk themselves and leave the pack stage unset; the
  file they write has an empty chunk in its place.
- **A fix:** after the check passes, take the channels' packed size as the chunk's, so the
  chunk is compressed and written from the caller's packed buffer. The check itself (which
  compares the size with `compressed_bytes`) is kept as upstream has it.
- **Status:** **fixed**, deviation D-16 (owner, 2026-11-07) (`crates/openexr-core/src/encoding.rs`);
  checked by `encoding::tests::caller_packed_chunks_are_written`.

### I-21. The write side's tile chunk info ignores the level's size

- **Upstream:** `exr_write_tile_chunk_info` clips the last tiles of a row or column against
  the data window, which is the size of level 0, so a tile at the edge of a smaller mipmap or
  ripmap level keeps the whole tile size (`src/lib/OpenEXRCore/chunk.c:2005-2027` @ v3.5.3);
  `exr_read_tile_chunk_info` clips against the level's own size (`chunk.c:1175-1193`).
- **Who notices:** writers of mipmapped or ripmapped tiled parts through Core: the chunk info
  of such tiles is too large, the encode pipeline packs a whole tile, and the file written
  does not read back (the reader refuses the chunk's sizes).
- **A fix:** clip as the read side does. Level 0 gives the same sizes both ways.
- **Status:** **fixed**, deviation D-17 (owner, 2026-11-07, "anything that is a bug we can
  fix") (`crates/openexr-core/src/chunk_write.rs`); checked by
  `chunk_write::tests::decreasing_mipmapped_tiles_go_in_the_cpp_librarys_order`, whose
  mipmapped file reads back tile by tile.

### I-22. DWA's quantizer counts bits differently on Windows

- **Upstream:** `countSetBits` is `__builtin_popcount` where `__has_builtin` reports it, and
  otherwise a multiply trick (`src/lib/OpenEXRCore/internal_dwa_encoder.h:266-288` @ v3.5.3).
  MSVC has no `__has_builtin` (checked with MSVC 19.44, the Windows wheel's compiler), so the
  Windows wheel takes the trick, which loses bit 15 of its argument. The quantizer's rounded
  candidate `(abssrc + npow2) & mask` can reach 0x8000 or more (a negative half) for a
  coefficient of 32768 or more; as a negative value it always passes the tolerance test, and
  it wins whenever it has fewer bits set than the source, its sign bit uncounted on Windows.
  The coefficient then becomes a negative zero or a tiny negative value, more often on
  Windows, so the two wheels can write different DWA bytes for very bright pixels. The test
  copy of the quantizer (`compressionTables.cpp`) counts the same way; its table form then
  counts a negative source's sign bit as unset, which is why its comparison reports
  mismatches on Windows (it reports them and does not fail).
- **Who notices:** writers of DWAA/DWAB files who compare Windows and Linux output.
- **The port:** follows each wheel: the multiply trick with an MSVC target, the population
  count elsewhere (`internal_dwa_encoder.rs`, and the test's copy in
  `upstream_tests/compression_tables.rs`).
- **A fix:** keep the rounded candidate below 0x8000, and count all 16 bits on every
  platform. It would change both wheels' bytes for such pixels, so it waits for the owner.
- **Status:** matched; listed for the owner's review.

### I-23. B44 and PXR24 keep a result exactly the packed size

- **Upstream:** the B44 and PXR24 compressors fall back to the packed bytes only when their
  result is larger (`nOut > encode->packed_bytes`,
  `src/lib/OpenEXRCore/internal_b44.c:496-504` and `internal_pxr24.c:230-238` @ v3.5.3); RLE,
  ZIP, PIZ and DWA fall back when it is not smaller, as the C++ library does. A chunk whose
  size is its unpacked size is read as raw data (`compression.c:676-692`), so a B44 or B44A
  chunk without half channels (whose "compressed" data is its channels' bytes, reordered by
  channel) is written at exactly that size and reads back scrambled, by Core and by the C++
  library; so would a PXR24 stream of exactly the packed size.
- **Who notices:** writers of B44, B44A or PXR24 files through Core: a part without half
  channels written with B44 (found by the heavy check of the lossy encoders: 814 of the 851
  values of each channel of a float-only image differ when read back).
- **A fix:** fall back when the result is not smaller, as the other codecs and the C++
  library do. The files written then read back, and equal the wheel's.
- **Status:** **fixed**, deviation D-18 (owner, 2026-11-07, "anything that is a bug we can
  fix") (`crates/openexr-core/src/internal_b44.rs`, `internal_pxr24.rs`); checked by
  `heavy_decode::images_encode_from_their_pixels_as_the_wheel_wrote_them`.

### I-24. OpenJPH gives a component's quantization step to the default when it has no QCC

- **Upstream:** `param_qcd::set_delta (comp_idx, delta)` looks for the component's QCC with
  `get_qcc`, which answers the QCD itself when there is none, never NULL, so the step goes to
  the QCD (`external/OpenJPH/src/core/codestream/ojph_params.cpp:2029-2036` @ OpenJPH 0.32.0).
  OpenEXRCore's LJ2K encoder sets the Y step on the QCD, then the Cb and the Cr steps on
  components 1 and 2 (`src/lib/OpenEXRCore/internal_ht.cpp:819-841` @ v3.5.3), so every
  component is quantized with the Cr step, and the Y and Cb steps are never used.
- **Who notices:** writers of LJ2K files: the luminance is quantized more coarsely than
  intended (the Cr step is the Y step divided by `sqrt (2.4756 / 3)`, about 1.1).
- **A fix:** add the component's QCC when it has none, as `set_qfactor (comp_idx, ...)` does.
  It changes the LJ2K files both wheels write, so it waits for the owner.
- **Status:** matched (`crates/openexr-openjph/src/params.rs`); listed for the owner's review.

### I-25. OpenJPH's 64-bit block decoder shifts its SigProp value in 32 bits

- **Upstream:** the significance propagation pass of `ojph_decode_codeblock64` sets a new
  sample to `ui64 val = 3u << (p - 2)`, whose shift is of a 32-bit `unsigned`; with `p` up to
  62 the count is often 32 or more, which C leaves undefined and x86-64's `shl` takes modulo
  32 (`external/OpenJPH/src/core/coding/ojph_block_decoder64.cpp:1549` @ OpenJPH 0.32.0).
- **Who notices:** readers of HTJ2K codeblocks of more than 30 bit planes that carry a SigProp
  pass; OpenJPH's encoder, and so OpenEXR, writes the cleanup pass only.
- **A fix:** `3ULL << (p - 2)`. It changes no file OpenEXR writes; it waits for the owner.
- **Status:** matched, as x86-64 computes it (`crates/openexr-openjph/src/block_decoder64.rs`);
  listed for the owner's review.

### I-26. OpenJPH shifts by 31 - K_max in 32 bits

- **Upstream:** an irreversible subband's step is divided by `(float)(1u << (31 - K_max))`
  (`external/OpenJPH/src/core/codestream/ojph_subband.cpp:117-221` @ OpenJPH 0.32.0); a
  `K_max` over 31 shifts by 32 or more, which C leaves undefined and x86-64's `shl` takes
  modulo 32.
- **Who notices:** readers of crafted irreversible HTJ2K codestreams; OpenEXR's `K_max` is at
  most 31 for its floats' 32-bit samples.
- **Status:** matched, as x86-64 computes it (`crates/openexr-openjph/src/subband.rs`).

### I-27. Core's planar HTJ2K encoder skips a component by the wrong channel's height

- **Upstream:** the planar loop of `ht_apply_impl` skips codestream component `c` when
  `encode->channels[c].height` is 0, a file channel's index used for a codestream component's
  (`src/lib/OpenEXRCore/internal_ht.cpp:945-947` @ v3.5.3); with R, G and B reordered first,
  the two differ, and a component could be skipped, or not, by another channel's height.
- **Who notices:** writers of subsampled HTJ2K parts with RGB channels and a channel of no rows
  in a chunk; the decoder checks the file channel.
- **Status:** matched (`crates/openexr-core/src/internal_ht.rs`); listed for the owner's review.

### I-28. OpenJPH checks a comment's previous length

- **Upstream:** `comment_exchange::set_string` refuses a string longer than 65531 bytes by
  testing `len`, the object's previous length, instead of the new string's
  (`external/OpenJPH/src/core/codestream/ojph_params.cpp:480-489` @ OpenJPH 0.32.0), so a
  longer string is taken and its length truncated to 16 bits.
- **Who notices:** no one through OpenEXR, whose comment is short.
- **Status:** matched (`crates/openexr-openjph/src/codestream.rs`); listed for the owner's
  review.

### I-29. Core compresses an unfilled buffer as a deep chunk's sample count table

- **Upstream:** when `exr_compress_chunk` is given a deep chunk's sample count table to
  compress (`skip_sample_count_table_compression` unset), it points the packed buffer at the
  freshly allocated `packed_sample_count_table`, compresses that uninitialized buffer into the
  compressed buffer (which the data's compression then overwrites), and leaves
  `packed_sample_count_bytes` as it was (`src/lib/OpenEXRCore/compression.c:558-622` @ v3.5.3);
  the chunk is written with no table. "This is never called in regular c++ usage": the C++
  library compresses the table itself and skips this.
- **Who notices:** writers of compressed deep files through Core alone, whose files do not read
  back.
- **A fix:** compress the table as the C++ library's deep writers do through Core
  (`ImfDeepScanLineOutputFile.cpp:690-760`, `ImfDeepTiledOutputFile.cpp:940-1030`): as a plain
  buffer, kept raw when it does not shrink.
- **Status:** **fixed**, deviation D-19 (owner, 2026-11-07: "anything that is a bug we can
  fix") (`crates/openexr-core/src/compression.rs`); Core alone now writes the chunks the wheel
  writes, byte for byte (`tests/heavy_deep.rs`).

## Messages and debug output

### I-7. A deep tile's unpacked size error prints the packed size

- **Upstream:** `exr_read_tile_chunk_info` refuses a deep tile's unpacked size with
  "invalid unpacked size %" PRId64 but passes `ddata[1]`, the packed size, for it
  (`src/lib/OpenEXRCore/chunk.c:1412-1425` @ v3.5.3).
- **Who notices:** readers of corrupt deep tiled files, in the error text.
- **A fix:** print `ddata[2]`.
- **Status:** **fixed**, deviation D-3 (owner, 2026-11-07) (`crates/openexr-core/src/chunk_info.rs`),
  checked by `chunk_info::tests::deep_tile_unpacked_size_message`.

### I-8. The debug dump prints bytes and deepImageState attributes as unknown types

- **Upstream:** `print_attr` has no case for `EXR_ATTR_BYTES` or `EXR_ATTR_DEEP_IMAGE_STATE`,
  so valid attributes of both types print as `<ERROR Unknown type 'bytes'>`
  (`src/lib/OpenEXRCore/debug.c:258-260` @ v3.5.3).
- **Who notices:** readers of `exr_print_context_info (f, 1)` for parts with such attributes.
- **A fix:** print a bytes attribute as `%zu bytes`, with ` hint '%s'` when it has a type
  hint, and a deep image state as `%d (%s)` with the names of `Imf::DeepImageState` (messy,
  sorted, non-overlapping, tidy).
- **Status:** **fixed**, deviation D-4 (owner, 2026-11-07) (`crates/openexr-core/src/debug.rs`); checked by
  `debug::tests::dump_prints_every_type`.

### I-10. Three error messages are garbled

- **Upstream:** `exr_read_scanline_chunk_info` reports "Preparing read scanline ..." and,
  twice, "Preparing to scanline ..." (`src/lib/OpenEXRCore/chunk.c:977`, `1034`, `1069`), and
  `exr_attr_set_string` reports "Part type attribute must be implicitly only when adding a
  part" (`part_attr.c:2056`) @ v3.5.3.
- **Who notices:** readers of the error text.
- **A fix:** "Preparing to read scanline ..." and "Part type attribute must be set
  implicitly, only when adding a part".
- **Status:** **fixed**, deviation D-6 (owner, 2026-11-07) (`crates/openexr-core/src/chunk_info.rs`,
  `part_attr_set.rs`); the part type message is checked by
  `part_attr_set::tests::refusals_carry_messages`, the two deep scanline ones by
  `chunk_info::tests::deep_scanline_leader_messages`.

### I-13. Two decode errors print the channel index as a character

- **Upstream:** `exr_decoding_choose_default_routines` reports an unsupported output size or
  type "for channel %c (%s)", passing the channel's index `c` as an `int`: `%c` prints the byte
  of that value, a control character for the first channels
  (`src/lib/OpenEXRCore/decoding.c:403`, `414` @ v3.5.3).
- **Who notices:** readers of the error text for a channel set up with a bad
  `user_bytes_per_element` or `user_data_type`.
- **A fix:** print the index with `%d`.
- **Status:** **fixed**, deviation D-9 (owner, 2026-11-07): printed with `%d`
  (`crates/openexr-core/src/decoding.rs`); checked by
  `decoding::tests::bad_output_size_names_the_channel`.
### I-19. Two encode errors print the channel index as a character

- **Upstream:** `exr_encoding_run` reports an unsupported input size or type "for channel %c
  (%s)", passing the channel's index `c` as an `int`, as I-13 does on the decode side
  (`src/lib/OpenEXRCore/encoding.c:268-287` @ v3.5.3).
- **Who notices:** readers of the error text for a channel set up with a bad
  `user_bytes_per_element` or `user_data_type`.
- **A fix:** print the index with `%d`.
- **Status:** **fixed**, deviation D-15 (owner, 2026-11-07, as for I-13)
  (`crates/openexr-core/src/encoding.rs`); checked by
  `encoding::tests::bad_input_size_names_the_channel`.

## Undefined behaviour the port does not copy

### U-1. A string created from no data has an unset terminator

- **Upstream:** `exr_attr_string_create_with_length` with NULL data and a positive length
  allocates `len + 1` bytes but zeroes only `len` of them (`memset (outs, 0, len)`), so the
  byte at `str[len]` is whatever the allocator returned
  (`src/lib/OpenEXRCore/string.c:132-139` @ v3.5.3). `exr_attr_string_set_with_length` zeroes
  all `len + 1` (line 221).
- **Who notices:** code that reads such a string as a C string (`%s`, `strcmp`) past its
  `length` bytes; Core itself creates them only to fill them right away.
- **The port:** zeroes the whole buffer, so `str[len]` is 0. Nothing Core writes depends on
  the byte.
- **Status:** the port's choice; listed for the owner's review.

### U-2. An array shorter than its stated length

- **Upstream:** the attribute constructors take a pointer and a count and copy (or keep) that
  many elements: `exr_attr_float_vector_create`/`_init_static`
  (`src/lib/OpenEXRCore/float_vector.c:61-127`), `exr_attr_bytes_create` (hint and data,
  `bytes.c:70-101`) and `exr_attr_preview_create` (`preview.c:54-76`) @ v3.5.3, and the
  setters that copy into an attribute of the same size: `exr_attr_set_bytes`,
  `exr_attr_set_float_vector`, `exr_attr_set_preview` and `exr_attr_set_string_vector`
  (`part_attr.c:1294-1405`, `1612-1702`, `1882-1976`, `2156-2279`). An array with fewer
  elements is read past its end. Likewise `exr_read_chunk` and `exr_read_deep_chunk`
  (`chunk.c:1516-1672`) write past the end of a buffer smaller than the chunk.
- **Who notices:** callers passing a count larger than their array; C cannot tell.
- **The port:** the array is a slice, so its length is known; a slice shorter than the count
  is refused with `EXR_ERR_INVALID_ARGUMENT` and its default message.
- **Status:** the port's choice; listed for the owner's review.

### U-3. A new m44d attribute is half zeroed

- **Upstream:** `attr_init` zeroes an `EXR_ATTR_M44D` value as an `exr_attr_m44f_t`, 64 of its
  128 bytes; the last eight doubles are what `alloc_fn` returned
  (`src/lib/OpenEXRCore/attributes.c:126-130` @ v3.5.3).
- **Who notices:** a caller that reads an m44d attribute it added before setting it; parsed
  attributes are filled from the file.
- **The port:** zeroes all sixteen doubles.
- **Status:** the port's choice; listed for the owner's review.

### U-4. exr_attr_set_channels on an attribute of another type

- **Upstream:** `exr_attr_set_channels` finds the attribute by name and, without checking its
  type, destroys `attr->chlist` and stores a channel list there
  (`src/lib/OpenEXRCore/part_attr.c:1446-1466` @ v3.5.3). On an attribute of another type
  that union member is another value read as a pointer: memory Core does not own is freed and
  written.
- **Who notices:** callers setting a channel list under a name already used by another type.
- **The port:** refuses it with `EXR_ERR_ATTR_TYPE_MISMATCH` and its default message, as the
  sibling setters refuse a type mismatch.
- **Status:** the port's choice; listed for the owner's review.

### U-5. exr_attr_set_user after a failed add, or with a built-in type name

- **Upstream:** `exr_attr_set_user` adds a missing attribute with
  `exr_attr_list_add_by_type` and then uses `attr->opaque` whatever the add returned
  (`src/lib/OpenEXRCore/part_attr.c:2531-2557` @ v3.5.3). After a failed add `attr` is NULL;
  with a built-in type name (`"int"`, `"box2i"`, ...) the attribute is not opaque and the
  union member read as `opaque` is another value.
- **Who notices:** callers passing an invalid name or type, or a built-in type name.
- **The port:** returns the add's error; a non-opaque attribute is refused with
  `EXR_ERR_ATTR_TYPE_MISMATCH` and its default message (the attribute stays added).
- **Status:** the port's choice; listed for the owner's review.

### U-6. Copying a required attribute name that has another type

- **Upstream:** `copy_attr`, under `exr_copy_unset_attributes`, adds a source attribute named
  `compression`, `dataWindow`, `displayWindow` or `lineOrder` with the source's type, and
  reads its value as that name's type to update the part (`srca->uc`, `*(srca->box2i)`)
  whatever the type is (`src/lib/OpenEXRCore/part_attr.c:257-325` @ v3.5.3); a copied
  `tiles` is read as a tile descriptor by `internal_exr_compute_tile_information` after the
  loop (line 577). A write
  context can hold such an attribute: `exr_attr_set_int (f, 0, "compression", 1)` adds an int
  named `compression`. Reading another type's union member reads a pointer's byte, or follows
  an int as a pointer.
- **Who notices:** callers copying from a part that holds a required name with a wrong type.
- **The port:** refuses the copy with `EXR_ERR_ATTR_TYPE_MISMATCH` and its default message,
  before adding anything; the attributes copied before it stay.
- **Status:** the port's choice; listed for the owner's review.

### U-7. A tile index past 2^31 within its level

- **Upstream:** `validate_and_compute_tile_chunk_off` adds `tiley * numx + tilex` to the chunk
  offset in `int` (`src/lib/OpenEXRCore/chunk.c:109` and `170` @ v3.5.3), which overflows for
  a level of more than 2^31 tiles; it then checks only `chunkoff >= chunk_count`. A header
  whose chunk count wrapped (I-4) can pass a negative offset, and the chunk table is read
  before its start.
- **Who notices:** crafted files with huge data windows and small tiles.
- **The port:** wraps the sum as the wheels' code does, and refuses a negative offset with the
  same `EXR_ERR_UNKNOWN` "Invalid tile chunk offset" error.
- **Status:** the port's choice; listed for the owner's review.

### U-8. States header validation already excludes

- **Upstream:** `read_and_validate_chunk_leader` divides by the part's lines per chunk
  (`src/lib/OpenEXRCore/chunk.c:381` @ v3.5.3), a hardware trap for 0, and
  `validate_and_compute_tile_chunk_off` indexes the tile level arrays by levels it has only
  compared with the level counts (lines 37-160). Header parsing and the tile information
  never leave 0 lines per chunk, nor arrays shorter than the counts.
- **Who notices:** nobody through the API; the checks keep the port from panicking should an
  internal state ever break them.
- **The port:** refuses 0 lines per chunk with `EXR_ERR_BAD_CHUNK_LEADER`, and level arrays
  shorter than the level counts with the existing "Tile descriptor data missing or corrupt".
- **Status:** the port's choice; listed for the owner's review.

### U-9. Decoding outside the caller's buffers

- **Upstream:** the decode pipeline writes each channel at `decode_to_ptr` plus the pixel
  and line strides, and reads the unpacked data by the channels' sizes, without bounds
  (`src/lib/OpenEXRCore/decoding.c:92-150`, `unpack.c:209-1299` @ v3.5.3); a layout that
  reaches outside the caller's memory writes outside it.
- **Who notices:** callers whose strides or buffers do not fit the chunk.
- **The port:** a channel names one of the run's buffers and an offset into it
  (`crates/openexr-core/src/coding.rs`, `BufferRef`). Before writing anything, each of the
  pipeline's own stages (the direct read and every unpack routine) works out the whole set
  of bytes it will write, with the routine's own pattern (the interleaved paths, for
  instance, start from channel 0's or channel `n - 1`'s position and advance by channel 0's
  line stride), and refuses a layout that reaches outside its buffer with
  `EXR_ERR_INVALID_ARGUMENT` and "Channel %d (%s): the output reaches outside buffer %d
  (bytes %lld to %lld; the buffer holds %llu)", leaving the buffers untouched; every store is
  bounds-checked as well. Layouts inside the buffers, negative strides included, decode as
  upstream does, byte for byte. Checked by `unpack::tests` (a buffer one byte short, a
  bottom-up tile, each fast path against `generic_unpack`). The deep unpackers check the same
  way: each channel's run of samples, or each pixel's samples at its pointer; a table of
  pointers (`EXR_DECODE_NON_IMAGE_DATA_AS_POINTERS`) is one of the run's buffers, each 8-byte
  entry a `BufferRef::to_pointer` value (0 for NULL), so that the strides count in
  `sizeof (void*)` as upstream's do.
- **Status:** the port's choice, made up front at the owner's request (2026-11-07).

### U-10. RLE coding writes up to a run past its output buffer

- **Upstream:** `internal_rle_compress` (behind `exr_rle_compress_buffer` and the RLE
  compressor) checks the output size only after writing each run, so the run that crosses
  the end of the buffer, up to 128 bytes, is written past it
  (`src/lib/OpenEXRCore/internal_rle.c:17-63` @ v3.5.3); the size it returns is then larger
  than the buffer.
- **Who notices:** callers of `exr_rle_compress_buffer` with a buffer smaller than the coded
  data; the RLE compressor's buffer is sized for its input (Phase 6 will say how much).
- **The port:** returns the same size and writes the same bytes inside the buffer, and
  nothing past it. Checked by `internal_rle::tests::rle_compress_stops_at_its_buffer`.
- **Status:** the port's choice; listed for the owner's review.

### U-11. exr_chunk_default_initialize divides by a zero tile size

- **Upstream:** `exr_chunk_default_initialize` divides by the tile width and height
  (`src/lib/OpenEXRCore/chunk.c:796-797` @ v3.5.3), which a write or temporary context can
  hold as 0 (`exr_set_tile_descriptor` stores any size); the division traps.
- **Who notices:** callers of the chunk initializer (the C++ library's compressors among
  them) on a part with a zero tile size.
- **The port:** refuses it with `EXR_ERR_INVALID_ARGUMENT`, as the function's other refusals.
- **Status:** the port's choice; listed for the owner's review.

### U-12. Encoding outside the caller's buffers

- **Upstream:** the pack stage reads each channel at `encode_from_ptr` plus the pixel and line
  strides, without bounds, and writes the packed buffer by the channels' sizes; the RLE stage
  and the write stage read the packed and compressed buffers by `packed_bytes` and
  `compressed_bytes` (`src/lib/OpenEXRCore/pack.c:20-188`, `internal_rle.c:93-124`,
  `encoding.c:31-94` @ v3.5.3). A layout that reaches outside the caller's memory reads
  outside it, and a channel without a pointer is read through NULL.
- **Who notices:** callers whose strides or buffers do not fit the chunk, or who change the
  pipeline's sizes or buffers between stages.
- **The port:** a channel names one of the run's input buffers and an offset into it. Before
  reading anything, the pack stage works out the whole set of bytes it will read and refuses
  a layout that reaches outside its buffer with `EXR_ERR_INVALID_ARGUMENT` and "Channel %d
  (%s): the input reaches outside buffer %d (bytes %lld to %lld; the buffer holds %llu)";
  every other read and write past a buffer, and a missing channel buffer, is refused with
  `EXR_ERR_INVALID_ARGUMENT`. Layouts inside the buffers encode as upstream does, byte for
  byte. Checked by `encoding::tests::input_one_byte_short_is_refused`.
- **Status:** the port's choice, as U-9.

### U-13. DWA reads past its AC buffer, and trusts its row layout

- **Upstream:** `DwaCompressor_uncompress` checks the AC count against the AC buffer only when
  the chunk has compressed AC data (`src/lib/OpenEXRCore/internal_dwa_compressor.h:959-967`
  @ v3.5.3); a chunk that claims AC values without any makes the lossy decoders read the
  buffer as far as the count says, past its end (`internal_dwa_decoder.h:138`). The decoders
  and the RLE and UNKNOWN copies write each channel's rows where the row layout puts them,
  without checking them against the unpacked buffer (`internal_dwa_decoder.h:590-776`,
  `internal_dwa_compressor.h:1245-1322`).
- **Who notices:** readers of corrupt DWAA/DWAB chunks.
- **The port:** the decoders read the AC buffer as far as it goes and refuse a chunk whose AC
  values run past it with `EXR_ERR_CORRUPT_CHUNK`; every read and write outside the planar,
  RLE and unpacked buffers is refused the same way. Chunks that stay inside their buffers
  decode as upstream decodes them, bit for bit.
- **Status:** the port's choice, as U-9.

### U-14. The DWA compressor goes on after its buffers fail to allocate

- **Upstream:** `DwaCompressor_compress` stores the result of `DwaCompressor_initializeBuffers`
  and overwrites it with the next call's before checking it
  (`src/lib/OpenEXRCore/internal_dwa_compressor.h:260-273` @ v3.5.3), so a chunk whose AC, DC,
  RLE or planar buffers could not be allocated is compressed through NULL pointers.
- **Who notices:** writers of DWAA/DWAB chunks too large for the memory at hand.
- **The port:** returns the allocation's `EXR_ERR_OUT_OF_MEMORY`. Every read and write the
  compressor makes outside its buffers is refused with `EXR_ERR_INVALID_ARGUMENT`, as U-12.
- **Status:** the port's choice, as U-9.

### U-15. OpenJPH reads an NLT segment past its end

- **Upstream:** `param_nlt::read` reads each field of an NLT segment and goes on when a read
  comes up short, using the field's uninitialized bytes; the codestream ignores its result
  (`external/OpenJPH/src/core/codestream/ojph_params.cpp:2596-2718` and
  `ojph_codestream_local.cpp:863` @ OpenJPH 0.32.0). For a LUT of 1-byte points it reads the
  size of the whole table's storage into the points, which lie at the storage's end, so it
  writes past the allocation and swallows the bytes after the segment.
- **Who notices:** readers of truncated or crafted HTJ2K chunks; OpenEXR writes 2- and 4-byte
  points only.
- **The port:** refuses the segment (an OpenJPH error with code 0 and no message), which
  Core reports as `EXR_ERR_CORRUPT_CHUNK`.
- **Status:** the port's choice, as U-9.

### U-16. OpenJPH follows a missing DFS segment when sizing a component

- **Upstream:** `param_siz::get_recon_downsampling` looks up the DFS segment a COD or COC names
  and uses it without checking it was found
  (`external/OpenJPH/src/core/codestream/ojph_params.cpp:949-962` @ OpenJPH 0.32.0), so a
  codestream naming a DFS index it lacks makes `get_recon_width`/`get_recon_height`, which Core
  calls right after reading the headers, dereference NULL.
- **Who notices:** readers of crafted HTJ2K chunks; OpenEXR writes no DFS segment.
- **The port:** refuses it (an OpenJPH error), which Core reports as
  `EXR_ERR_CORRUPT_CHUNK`. The resolutions' own DFS checks (codes 0x00070001/2 and
  0x00070011/2) are ported as upstream's.
- **Status:** the port's choice, as U-9.

### U-17. OpenJPH's block decoders write the row after an odd-height codeblock

- **Upstream:** the HT block decoders decode rows in pairs and write the second row of the last
  pair even when the codeblock's height is odd
  (`external/OpenJPH/src/core/coding/ojph_block_decoder32.cpp:1155-1309` @ OpenJPH 0.32.0, and
  `ojph_block_decoder64.cpp:1203-1312`); the codeblock's buffer is `stride * nominal height`
  (`ojph_codeblock.cpp:72-112`), so a codeblock of the nominal height with an odd one (1, with
  precincts of 2 rows at a level) writes a row into the next allocation of the codestream.
- **Who notices:** readers of crafted HTJ2K chunks; OpenEXR's codeblocks are 32 rows high.
- **The port:** gives each codeblock's buffer a spare row, which takes the write; the samples
  pulled are the same.
- **Status:** the port's choice, as U-9.

### U-18. OpenJPH addresses a 64-bit irreversible codeblock's line as 64-bit samples

- **Upstream:** `codeblock::pull_line` hands the 64-bit conversion `line->i64 + line_offset`,
  but an irreversible line holds floats, so the floats land at `2 line_offset` and the clearing
  of a zero codeblock covers `2 cb_size.w` floats
  (`external/OpenJPH/src/core/codestream/ojph_codeblock.cpp:230-266` @ OpenJPH 0.32.0); past
  the first codeblock of a row it writes outside its place, and past the line's end outside
  the line.
- **Who notices:** readers of irreversible HTJ2K codestreams of more than 30 bit planes, which
  OpenEXR does not write.
- **The port:** writes where upstream writes while that is inside the line's storage, and
  refuses (an OpenJPH error) past it.
- **Status:** matched where upstream stays in its memory; the port's choice past it, as U-9.

### U-19. OpenJPH's block encoder reaches past its line arrays for 1024-wide codeblocks

- **Upstream:** `ojph_encode_codeblock32/64` keep a row's exponents and contexts in arrays of
  513 bytes (`e_val`, `cx_val`), two samples a byte plus one; for a codeblock 1024 samples wide
  the first row writes the 514th byte and the next rows read it, and the 514th byte of `cx_val`
  is read without being written
  (`external/OpenJPH/src/core/coding/ojph_block_encoder.cpp:542-1023` @ OpenJPH 0.32.0).
- **Who notices:** writers of codestreams with 1024-wide codeblocks; OpenEXR's are 128 wide.
- **The port:** gives the arrays room for it, zeroed, which is what the bytes would hold if
  nothing else wrote them.
- **Status:** the port's choice, as U-9.

### U-20. OpenJPH has no conversion for an irreversible codeblock of 64-bit samples

- **Upstream:** `codeblock_fun::init` sets `tx_to_cb64` to NULL for an irreversible
  codeblock (`external/OpenJPH/src/core/codestream/ojph_codeblock_fun.cpp:147-341`
  @ OpenJPH 0.32.0), and `codeblock::push` calls it for one whose precision exceeds 32 bits.
- **Who notices:** writers of irreversible codestreams of more than 30 bit planes; OpenEXR's
  lossy components are coded in 32 bits.
- **The port:** refuses the codeblock (an OpenJPH error), which Core reports as
  `EXR_ERR_INCORRECT_CHUNK`.
- **Status:** the port's choice, as U-9.
