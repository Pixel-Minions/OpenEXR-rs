# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Copies a single-part HTJ2K or LJ2K image with its wavelet replaced by an arbitrary
reversible kernel (an ATK marker segment, JPEG 2000 Part 2), for the heavy-tier decode checks
of OpenJPH's reversible lifting with any ATK-Eatk, ATK-Batk and ATK-Aatk.

  uv run --project oracle python -I oracle/craft_atk.py <in.exr> <out.exr> <e,b,a>...

Each <e,b,a> is one lifting step: ATK-Eatk (0 to 255), ATK-Batk and ATK-Aatk (-32768 to
32767), in the order of the segment's steps (OpenJPH's `lifting_step` d[0], d[1], ...; its
5/3 kernel is `2,2,1 1,1,-1`). In each chunk the codestream (after Core's HTJ2K chunk header,
internal_ht_common.cpp `read_header`) gets an ATK segment right after its COD segment, with
index 2, 16-bit coefficients, whole-sample, reversible, symmetric extension (ATK-Satk 0x5902),
and COD's (and any COC's) wavelet kernel set to it. The chunk's size grows by the segment's
length and the offset table is rewritten; a chunk stored raw (no codestream) is copied as it
is. Nothing else changes: the image decodes with the given kernel instead of the one its
coefficients were coded with, which is what the checks need.

This script writes inputs; it reports nothing and computes no expected values.
"""

import struct
import sys

ATK_INDEX = 2
SATK = 0x4000 | 0x1000 | 0x0800 | (1 << 8) | ATK_INDEX


def header_end(data):
    """The offset just past the header's attributes, and whether the part is tiled."""
    if data[:4] != b"\x76\x2f\x31\x01":
        sys.exit("not an OpenEXR file")
    flags = struct.unpack_from("<I", data, 4)[0]
    if flags & 0x1000:
        sys.exit("multi-part files are not supported")
    if flags & 0x800:
        sys.exit("deep files are not supported")
    tiled = bool(flags & 0x200)
    pos = 8
    while data[pos] != 0:
        name_end = data.index(b"\0", pos)
        type_end = data.index(b"\0", name_end + 1)
        size = struct.unpack_from("<i", data, type_end + 1)[0]
        pos = type_end + 5 + size
    return pos + 1, tiled


def atk_segment(steps):
    natk = len(steps)
    body = struct.pack(">HB", SATK, natk)
    for e, b, a in steps:
        # Eatk, Batk, LCatk (1), Aatk
        body += struct.pack(">BhBh", e, b, 1, a)
    return struct.pack(">HH", 0xFF79, 2 + len(body)) + body


def recode(cs, steps):
    """The codestream `cs` with the ATK segment and its kernel."""
    if cs[:2] != b"\xff\x4f":
        raise ValueError("no SOC")
    out = bytearray(cs[:2])
    pos = 2
    inserted = False
    csiz = 0
    while True:
        marker = struct.unpack_from(">H", cs, pos)[0]
        if marker == 0xFF90:  # SOT: the main header ends
            break
        length = struct.unpack_from(">H", cs, pos + 2)[0]
        seg = bytearray(cs[pos : pos + 2 + length])
        if marker == 0xFF51:  # SIZ: Lsiz Rsiz Xsiz Ysiz XOsiz YOsiz XTsiz YTsiz XTOsiz YTOsiz Csiz
            csiz = struct.unpack_from(">H", seg, 38)[0]
            out += seg
        elif marker == 0xFF52:  # COD: Lcod Scod SGcod(4) SPcod: decomp, xcb, ycb, style, kernel
            seg[13] = ATK_INDEX
            out += seg
            out += atk_segment(steps)
            inserted = True
        elif marker == 0xFF53:  # COC: Lcoc Ccoc(1 or 2) Scoc SPcoc: decomp, xcb, ycb, style, kernel
            seg[11 if csiz >= 257 else 10] = ATK_INDEX  # (Ccoc is 16-bit from 257 components)
            out += seg
        else:
            out += seg
        pos += 2 + length
    if not inserted:
        raise ValueError("no COD")
    out += cs[pos:]
    return bytes(out)


def recode_chunk(payload, steps):
    """A chunk's HTJ2K payload with the ATK segment, or None when it holds no codestream."""
    if len(payload) < 6:
        return None
    payload_sz = struct.unpack_from(">I", payload, 2)[0]
    header_sz = 6 + payload_sz
    if payload[header_sz : header_sz + 2] != b"\xff\x4f":
        return None
    return payload[:header_sz] + recode(payload[header_sz:], steps)


def main():
    if len(sys.argv) < 4:
        sys.exit(__doc__)
    src, dst = sys.argv[1], sys.argv[2]
    steps = [tuple(int(v) for v in s.split(",")) for s in sys.argv[3:]]
    for e, b, a in steps:
        if not (0 <= e <= 255 and -32768 <= b <= 32767 and -32768 <= a <= 32767):
            sys.exit(f"step out of range: {e},{b},{a}")
    data = open(src, "rb").read()
    table, tiled = header_end(data)
    first = struct.unpack_from("<Q", data, table)[0]
    count = (first - table) // 8
    offsets = list(struct.unpack_from(f"<{count}Q", data, table))
    lead = 20 if tiled else 8  # (tile x, y, level x, y, size) or (y, size)
    chunks = []
    recoded = 0
    for off in offsets:
        size = struct.unpack_from("<i", data, off + lead - 4)[0]
        payload = data[off + lead : off + lead + size]
        new = recode_chunk(payload, steps)
        if new is not None:
            payload = new
            recoded += 1
        chunks.append(data[off : off + lead - 4] + struct.pack("<i", len(payload)) + payload)
    if recoded == 0:
        sys.exit("no chunk holds a codestream")
    out = bytearray(data[:table])
    pos = first
    new_offsets = []
    for c in chunks:
        new_offsets.append(pos)
        pos += len(c)
    out += struct.pack(f"<{count}Q", *new_offsets)
    for c in chunks:
        out += c
    open(dst, "wb").write(out)


if __name__ == "__main__":
    main()
