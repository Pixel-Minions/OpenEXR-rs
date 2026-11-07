# Improvement candidates

Upstream bugs and limitations that the port copies, so that its results match OpenEXR 3.5.3
byte for byte, but that could be fixed. Nothing here is fixed during the port. At the end, the
owner decides on each one: keep it, or fix it as a deviation (`docs/deviations.md`), which
changes the port's results for the inputs the entry names. The rules are OpenColorIO-rs's
(`CLAUDE.md`, rule 1).

**Adding an entry.** A chunk that ports an upstream bug or limitation, or a platform
difference that exists only because of how the wheels were built, adds an entry here in the
same chunk. Numbers are never changed or reused: take the next free one in the series (`I-`
for behaviour copied, `U-` for undefined behaviour the port refuses instead), whatever the
section. An entry says:
- **Upstream:** what OpenEXR does, with its source (`file:line @ v3.5.3`) or the oracle probe
  that showed it;
- **Who notices:** the inputs or calls affected, in plain words;
- **A fix:** what the port would do instead, and what that changes;
- **Status:** matched (with the chunk), to be matched (with the phase), or open; and how it is
  verified.

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

### U-2. A float array shorter than its stated length

- **Upstream:** `exr_attr_float_vector_create` and `exr_attr_float_vector_init_static` take an
  array and a count and read (or keep a pointer to) `nent` floats from it
  (`src/lib/OpenEXRCore/float_vector.c:61-127` @ v3.5.3); an array with fewer is read past its
  end.
- **Who notices:** callers passing a count larger than their array; C cannot tell.
- **The port:** the array is a slice, so its length is known; a slice shorter than `nent` is
  refused with `EXR_ERR_INVALID_ARGUMENT` and its default message.
- **Status:** the port's choice; listed for the owner's review.
