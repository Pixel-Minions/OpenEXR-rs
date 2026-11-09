# Proposal: the next milestone after M3 (draft, 2026-11-09)

M3 is done: OpenEXRCore 3.5.3 and its three libraries are ported, at byte-for-byte parity
(`docs/m3.md`). What the port lacks is a way for an ordinary Rust program to use it. Today's
API is Core's C API in Rust form: a caller opens a context, asks for chunk infos, sets up decode
pipelines with buffer references, strides and per-channel types, and runs them chunk by chunk.
That is what Core offers C callers, and what OpenEXR's own C++ library is built on.

Two directions, not exclusive. This document sizes both and recommends an order; the owner
decides (`PLAN.md` §14).

## Option A: a safe, idiomatic Rust API on top of the Core port

**What it is.** A new crate, `openexr-rs` (name open), that reads and writes whole images
with no raw pointers, no manual chunk loops and no pipeline setup:

- Open a file (path, `Read + Seek`, or bytes); list its parts, each with its header as typed
  attributes (the standard ones as fields, the rest as an attribute map).
- Read a part's pixels into planes the API allocates, or into caller buffers described by a
  safe `Slice` (an element type, a `&mut [u8]` and strides, checked once), whole or by line
  and tile ranges, mipmap and ripmap levels included, with type conversion (half, float, uint).
- Read deep parts: sample counts, then the samples, per channel.
- Write single- and multi-part files, scanline and tiled, flat and deep, with any of the 14
  compressions and their levels, from the same `Slice`s.
- Errors as one `Error` enum carrying Core's code and message.
- Optionally, chunks decoded and encoded on several threads (each chunk is independent; the
  bytes don't change), behind a feature, if the owner allows a dependency (`rayon`) or with
  `std::thread::scope` alone.

Everything below the API is the verified Core port, unchanged. The API adds no pixel
arithmetic of its own beyond Core's conversions, so parity carries over.

**How it is checked.** The wheel's own high-level API (`OpenEXR.File`, read and write) is the
oracle: the same images read through both give the same pixels, and the same pixels written
through both give the same files, byte for byte, across `write_images.py`'s layouts and the
corpus. Plus unit tests for the API's own checks (stride and size validation, type errors).

**Size.** Not a port, a design: perhaps 6,000 to 10,000 Rust lines with tests. About 6 to 10
conventional person-weeks; at this project's pace, roughly one to two weeks of calendar.

**What it does not give.** The C++ library's extra features: the RGBA interface with its
luminance/chroma (YCA) conversion, ACES files, multi-view helpers, deep compositing, ID
manifests, the in-memory image classes of OpenEXRUtil, preview generation, and its exact
behaviour in corner cases where it differs from Core.

**Risks.** Design taste: an API is a commitment once published. Mitigation: model the names and
concepts on the C++ library's (`Header`, `FrameBuffer`, `Slice`, `InputPart`, `OutputPart`,
`DeepFrameBuffer`), so users of OpenEXR find what they expect, and keep it unpublished until the
owner signs off on it.

## Option B: port the OpenEXR C++ library (Imf), exactly

**What it is.** The C++ library of OpenEXR 3.5.3, with the same exactness rules as Core
(`CLAUDE.md`):

| Upstream | Size | What it is |
|---|---|---|
| `src/lib/OpenEXR` | 99 sources, 118 headers, ~56K lines | Files and parts (scanline, tiled, deep, multi-part), `Header` and the attribute classes, `FrameBuffer`, the RGBA and tiled RGBA interfaces with YCA, ACES, multi-view, previews, LUTs, deep compositing, ID manifests, threading, and the compressors as thin wrappers over Core |
| `src/lib/OpenEXRUtil` | 15 sources, ~8K lines | In-memory flat and deep images (`ImfImage`), and `checkOpenEXRFile` |
| `src/lib/Iex`, `src/lib/IlmThread` | ~4.8K lines | Exceptions and the thread pool, which map to Rust's `Result` and threads |
| `src/test/OpenEXRTest` and others | 64 test files, ~31K lines | Upstream's tests, ported as Core's were |

Since 3.x the C++ library decodes and compresses through Core (its compressors are wrappers of
a few dozen lines, its input files read through `ImfContext`), so this ports mostly the layer
above: the classes, the RGBA/YCA logic, the multi-part and deep bookkeeping, the output files
(which still assemble chunks in C++), and the threading.

**How it is checked.** The wheel exposes almost none of the C++ library (its binding calls
Core, plus `Imf::Header` and a few attributes), so this needs a second source-built harness,
like O1's: the C++ library built from the submodule with the wheel's settings, driven through
its API, accepted only where it reproduces the wheel. Then upstream's OpenEXRTest suite,
ported, and the harness for everything the tests leave out (YCA conversion, multi-view, deep
compositing, threaded reads matching serial ones).

**The hard part.** The C++ API is pointer-based: a `Slice` is a base pointer and strides
(`xStride`, `yStride`), and callers point it at arbitrary memory, including offsets from a data
window that begins at a negative coordinate. An exact Rust port can't keep raw pointers in a
safe API, so `Slice` becomes a checked view (as in option A), and every behaviour that depends
on pointer arithmetic is ported against that view. This is what option A designs anyway.

**Size.** By `PLAN.md` §11's rate (~1.5K non-blank lines per person-week, less for the subtle
parts): about 30 person-weeks for the library, 5 for OpenEXRUtil, 3 for Iex and IlmThread, 5 for
the harness, and integration: ~45 person-weeks (range 35 to 60). At this project's pace, roughly
two to three weeks of calendar.

**Risks.** The second harness, its acceptance against the wheel on both platforms, and the
pointer-based API's corner cases. Exact parity for threaded paths (ordering of errors and
messages when several chunks fail).

## Option C: both, A first

1. **M4, the safe API** (option A), designed on the C++ library's concepts and names. Users get
   a usable crate in one to two weeks, checked against the wheel's high-level API.
2. **M5, the C++ library** (option B), ported beneath that API where it adds behaviour Core
   doesn't have (RGBA/YCA, ACES, multi-view, deep compositing, ID manifests, `ImfImage`), with
   its own harness. The API from M4 becomes the safe face of the ported classes, so M5 does not
   redesign it.

## Recommendation

**Option C.** A first, because it is small, it makes everything already verified usable, and
its oracle exists today (the wheel's `OpenEXR.File`). B second, and only as far as its features
are wanted: if no user needs YCA, ACES or deep compositing, the most valuable parts of B are
already covered by A plus Core.

## Decisions for the owner

1. The direction: A, B, or C (recommended).
2. For A: the crate's name (`openexr-rs`? `openexr`?), and whether it may depend on `rayon` for
   threaded decode and encode (or use `std` threads only, or stay single-threaded at first).
3. For A: whether the API follows the C++ library's names (recommended) or a new Rust-first
   design.
4. For B: whether the second harness (the C++ library built from source) is acceptable on the
   same terms as O1.
5. Publishing: whether any of the crates goes to crates.io, and when (`PLAN.md` O4 left names
   open until then).
