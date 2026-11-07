# Waivers

Every exception to the byte-exact definition of done (PLAN.md §3), with its cause and the
owner's sign-off. A check that compares with the oracle skips exactly the inputs a waiver
names, and says which waiver.

| Waiver | Output that differs | Inputs | Cause | Sign-off |
|---|---|---|---|---|
| W-1 | Which calls are refused | Windows: the enum setters given a negative value, now refused where the Windows wheel stores `(uint8_t) value` | Deviation D-1 (I-5) | Owner in chat, 2026-11-07 |
| W-2 | `exr_print_*` text | Boxes wider or taller than `INT_MAX`: their size | Deviation D-2 (I-6) | Owner in chat, 2026-11-07 |
| W-3 | Error text | Deep tiled chunks with an invalid unpacked size: the size the message prints | Deviation D-3 (I-7) | Owner in chat, 2026-11-07 |
| W-4 | `exr_print_*` text | Parts with `bytes` or `deepImageState` attributes: those attributes' lines | Deviation D-4 (I-8) | Owner in chat, 2026-11-07 |
| W-5 | Which calls are refused, and the attributes copied | `exr_copy_unset_attributes` from a part with a `deepImageState` attribute | Deviation D-5 (I-9) | Owner in chat, 2026-11-07 |
| W-6 | Error text | The three corrected messages of I-10 | Deviation D-6 (I-10) | Owner in chat, 2026-11-07 |
| W-7 | Error reports | `exr_set_version` with a version other than 1: a message is reported; the code is unchanged | Deviation D-7 (I-11) | Owner in chat, 2026-11-07 |
| W-8 | Error text | `exr_decoding_choose_default_routines` with an unsupported output size or type: the channel index | Deviation D-9 (I-13) | Owner in chat, 2026-11-07 |
| W-9 | Written files | Headers with `bytes` attributes: Core's layout is replaced by the C++ library's (the wheel writes headers with the C++ library, so against the wheel these now match) | Deviation D-10 (I-14) | Owner in chat, 2026-11-07 |
| W-10 | Which calls are refused | `exr_write_header` for multipart parts that disagree on a shared attribute | Deviation D-11 (I-15) | Owner in chat, 2026-11-07 |
| W-11 | Which calls are refused; written files | Headers with a `deepImageState` attribute | Deviation D-12 (I-16) | Owner in chat, 2026-11-07 |
| W-12 | Chunk info | `exr_write_scanline_chunk_info` for a line mid-chunk with y-subsampled channels, or a window ending near `INT_MAX` | Deviation D-13 (I-17) | Owner in chat, 2026-11-07 |
| W-13 | Which writes are refused; written files | Decreasing-Y scanline parts: chunks written bottom up (as the C++ library writes them) | Deviation D-14 (I-18) | Owner in chat, 2026-11-07 |

D-8 (I-12) has no waiver: no input reaches it, so no output differs.
