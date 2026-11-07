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

## Messages and debug output

### I-7. A deep tile's unpacked size error prints the packed size

- **Upstream:** `exr_read_tile_chunk_info` refuses a deep tile's unpacked size with
  "invalid unpacked size %" PRId64 but passes `ddata[1]`, the packed size, for it
  (`src/lib/OpenEXRCore/chunk.c:1412-1425` @ v3.5.3).
- **Who notices:** readers of corrupt deep tiled files, in the error text.
- **A fix:** print `ddata[2]`.
- **Status:** **fixed**, deviation D-3 (owner, 2026-11-07) (`crates/openexr-core/src/chunk_info.rs`). No
  test yet: it needs a corrupt deep tiled file (Phase 7).

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
  `part_attr_set::tests::refusals_carry_messages`.

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
  (`src/lib/OpenEXRCore/decoding.c:91-147`, `unpack.c:209-1299` @ v3.5.3); a layout that
  reaches outside the caller's memory writes outside it.
- **Who notices:** callers whose strides or buffers do not fit the chunk.
- **The port:** a channel names one of the run's buffers and an offset into it
  (`crates/openexr-core/src/coding.rs`, `BufferRef`); a store outside its buffer, or a read
  past the unpacked data, stops the stage with `EXR_ERR_INVALID_ARGUMENT`, which
  `exr_decoding_run` reports as upstream reports a failed stage. Every store inside the
  buffers lands where upstream's pointer arithmetic puts it, in the same order.
- **Status:** the port's choice; listed for the owner's review.
