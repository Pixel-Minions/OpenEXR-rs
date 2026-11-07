# Deviations from OpenEXR 3.5.3

Where the port intentionally behaves differently from upstream. Each entry needs the owner's
approval, and one that changes an output covered by the byte-exact definition of done
(PLAN.md §3) also has a waiver in `docs/waivers.md`. Each entry's `I-` or `U-` entry in
`docs/improvements.md` gives the upstream source, who notices, and how it is checked.

**Owner's decision (chat, 2026-11-07):** fix the upstream bugs found in Phases 2-4 that change
outputs (I-5 to I-12), as deviations; and I-13 (D-9), the same day.

| Id | Entry | Upstream behaviour | Port behaviour | Affects outputs? | Approved |
|---|---|---|---|---|---|
| D-1 | I-5 | The enum setters (`exr_set_lineorder`, `exr_attr_set_compression`, `_envmap`, `_lineorder`) compare in the enum's underlying type, so a negative value is refused on Linux and stored as `(uint8_t) value` on Windows | Refused on every platform with upstream's out-of-range message | Windows only: calls passing a negative enum value now fail | Owner in chat, 2026-11-07 |
| D-2 | I-6 | `exr_print_context_info` prints a box's size computed in `int`, which overflows past `INT_MAX` | Computed and printed in 64 bits | `exr_print_*` text, for boxes wider or taller than `INT_MAX` | Owner in chat, 2026-11-07 |
| D-3 | I-7 | A deep tile's "invalid unpacked size" error prints the packed size | Prints the unpacked size | Error text, for corrupt deep tiled files | Owner in chat, 2026-11-07 |
| D-4 | I-8 | `exr_print_context_info` prints `bytes` and `deepImageState` attributes as unknown types | Prints their values | `exr_print_*` text, for parts with such attributes | Owner in chat, 2026-11-07 |
| D-5 | I-9 | `exr_copy_unset_attributes` fails with `EXR_ERR_INVALID_ATTR` at a `deepImageState` attribute | Copies it | Calls copying a header with that attribute now succeed | Owner in chat, 2026-11-07 |
| D-6 | I-10 | Three garbled error messages ("Preparing read scanline", "Preparing to scanline", "must be implicitly only") | Corrected text | Error text | Owner in chat, 2026-11-07 |
| D-7 | I-11 | `exr_set_version` refuses a version other than 1 without a message | Reports one | Error text (the handler is called); the code is unchanged | Owner in chat, 2026-11-07 |
| D-8 | I-12 | `exr_copy_unset_attributes` counts a lookup failing for another reason than a missing name as found | Returns the lookup's error | None today (the lookup cannot fail that way with a source's names) | Owner in chat, 2026-11-07 |
| D-9 | I-13 | `exr_decoding_choose_default_routines` prints a channel's index with `%c`, as a character, in its two errors for an unsupported output size or type | Prints it with `%d` | Error text | Owner in chat, 2026-11-07 |
| D-10 | I-14 | Core writes a `bytes` attribute as size and data, without the hint length and hint the format holds, and refuses an empty one | Writes the hint length, hint and data, as the C++ library does; writes an empty one | Files Core writes with `bytes` attributes: they now read back, and match the C++ library's | Owner in chat, 2026-11-07 |
| D-11 | I-15 | `exr_write_header` writes multipart files whose parts disagree on a shared attribute | Refuses them with the reader's message | Which calls are refused | Owner in chat, 2026-11-07 |
| D-12 | I-16 | Core cannot write a `deepImageState` attribute (`EXR_ERR_INVALID_ATTR`) | Writes its byte, as the C++ library does | Which calls are refused; the files written | Owner in chat, 2026-11-07 |
| D-13 | I-17 | `exr_write_scanline_chunk_info` computes the unpacked size from the line given, not the chunk's first, and the window's end in `int` | Computes as the read side does | The chunk info for a line mid-chunk with y-subsampled channels | Owner in chat, 2026-11-07 |
| D-14 | I-18 | A `DECREASING_Y` file's chunks must be written in increasing chunk order, against the line order | In the C++ library's order: scanlines bottom up; tiles level by level, each level's rows bottom up | Which writes are refused, and the files written (now as the C++ library's) | Owner in chat, 2026-11-07 |
| D-15 | I-19 | `exr_encoding_run` prints a channel's index with `%c`, as a character, in its two errors for an unsupported input size or type | Prints it with `%d` | Error text | Owner in chat, 2026-11-07 (the rule for I-13, and "anything that is a bug we can fix") |
| D-16 | I-20 | An encode pipeline without a pack stage writes each chunk with 0 bytes of data | Writes the caller's packed bytes, compressed as the part says | The files such pipelines write | Owner in chat, 2026-11-07 ("anything that is a bug we can fix") |
| D-17 | I-21 | `exr_write_tile_chunk_info` clips tiles against the data window, not the level, so edge tiles of levels above 0 keep the whole tile size | Clips against the level, as the read side does | The chunk info of those tiles, and the files written with them (which now read back) | Owner in chat, 2026-11-07 ("anything that is a bug we can fix") |

