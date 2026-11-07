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
