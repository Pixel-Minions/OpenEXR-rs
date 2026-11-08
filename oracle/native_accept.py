# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""The source-built Core harness's acceptance (PLAN.md O1): it is used only where it reproduces
the official openexr==3.5.3 wheel, and this check is what shows that it does.

  uv run --project oracle python -I oracle/native_accept.py <core_harness> <work-dir> [<compression> ...]

For each compression (all of the wheel's by default), oracle/write_images.py writes its layouts
with the wheel, once uncompressed and once compressed. The wheel writes a layout's pixels from
the same seed whatever the compression, so the uncompressed file holds exactly the samples the
wheel compressed. The harness rewrites the uncompressed file through Core alone
(`core_harness recompress`, Core's default levels), and every chunk it writes, leader and data,
must equal the chunk of the same index in the wheel's compressed file, byte for byte.

Headers are not compared: the C++ library writes its attributes in its own order. Two upstream
bugs of Core's write side keep the harness from the wheel's bytes, and are reported apart, as
where the harness is not accepted (the port fixes both, docs/deviations.md):

- I-14: Core cannot write a bytes attribute (the "attrs" layout holds one), so the harness
  refuses the file;
- I-23: Core's B44 and B44A keep a result of exactly the packed size, so a part without half
  channels is stored reordered, where the wheel stores it raw.

This script reports what the two libraries wrote; it computes no expected values.
"""

import os
import struct
import subprocess
import sys

import OpenEXR

HERE = os.path.dirname(os.path.abspath(__file__))

COMPRESSIONS = [
    "NO_COMPRESSION",
    "RLE_COMPRESSION",
    "ZIPS_COMPRESSION",
    "ZIP_COMPRESSION",
    "PIZ_COMPRESSION",
    "PXR24_COMPRESSION",
    "B44_COMPRESSION",
    "B44A_COMPRESSION",
    "DWAA_COMPRESSION",
    "DWAB_COMPRESSION",
    "HTJ2K256_COMPRESSION",
    "HTJ2K32_COMPRESSION",
    "LJ2K_COMPRESSION",
    "ZSTD_COMPRESSION",
]


def headers(data):
    """The attributes of each part's header, as {name: (type, bytes)}, and where the offset
    tables start."""
    flags = struct.unpack_from("<I", data, 4)[0]
    multipart = bool(flags & 0x1000)
    pos, parts = 8, []
    while True:
        attrs = {}
        while True:
            end = data.index(b"\0", pos)
            name = data[pos:end]
            pos = end + 1
            if not name:
                break
            end = data.index(b"\0", pos)
            typ = data[pos:end]
            pos = end + 1
            size = struct.unpack_from("<i", data, pos)[0]
            pos += 4
            attrs[name.decode()] = (typ.decode(), data[pos:pos + size])
            pos += size
        if not attrs:
            break  # the empty header that ends a multipart file's headers
        parts.append(attrs)
        if not multipart:
            break
    return parts, multipart, flags, pos


def has_half_channel(path):
    """Whether the (single) part of the file has a half channel, from its chlist."""
    data = open(path, "rb").read()
    raw = headers(data)[0][0]["channels"][1]
    pos = 0
    while raw[pos] != 0:
        pos = raw.index(b"\0", pos) + 1
        if struct.unpack_from("<i", raw, pos)[0] == 1:  # HALF
            return True
        pos += 16
    return False


def chunks(path):
    """Each part's chunks, leader included, in chunk table order."""
    data = open(path, "rb").read()
    parts, multipart, flags, table = headers(data)
    deep = bool(flags & 0x800)
    if deep:
        raise SystemExit(f"{path}: deep files are not compared")
    if len(parts) > 1:
        counts = [struct.unpack("<i", p["chunkCount"][1])[0] for p in parts]
    else:
        # the table ends where its lowest chunk starts (a decreasing-Y table starts with its
        # last chunk): read entries until reaching the lowest offset read so far
        n, lowest = 0, len(data)
        while table + 8 * n < lowest:
            lowest = min(lowest, struct.unpack_from("<Q", data, table + 8 * n)[0])
            n += 1
        counts = [n]
    out, pos = [], table
    for p, n in zip(parts, counts):
        offs = struct.unpack_from(f"<{n}Q", data, pos)
        pos += 8 * n
        tiled = "tiles" in p
        lead = (4 if multipart else 0) + (16 if tiled else 4)
        part = []
        for o in offs:
            size = struct.unpack_from("<i", data, o + lead)[0]
            part.append(data[o:o + lead + 4 + size])
        out.append(part)
    return out


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    harness, work = os.path.abspath(sys.argv[1]), sys.argv[2]
    comps = sys.argv[3:] or COMPRESSIONS
    os.makedirs(work, exist_ok=True)
    writer = os.path.join(HERE, "write_images.py")
    subprocess.run([sys.executable, "-I", writer, work, "NO_COMPRESSION", *comps], check=True)

    layouts = sorted(f[: -len("_NO_COMPRESSION.exr")] for f in os.listdir(work) if f.endswith("_NO_COMPRESSION.exr"))
    failures, upstream_bugs, compared = [], [], 0
    for comp in comps:
        cid = int(getattr(OpenEXR, comp))
        for layout in layouts:
            src = os.path.join(work, f"{layout}_NO_COMPRESSION.exr")
            wheel = os.path.join(work, f"{layout}_{comp}.exr")
            mine = os.path.join(work, f"{layout}_{comp}.harness.exr")
            r = subprocess.run([harness, "recompress", src, mine, str(cid)], capture_output=True, text=True)
            if r.returncode != 0:
                if "Invalid reference to bytes attribute to save" in r.stderr:
                    upstream_bugs.append(f"{layout} {comp}: I-14 (a bytes attribute)")
                else:
                    failures.append(f"{layout} {comp}: the harness failed: {r.stderr.strip()}")
                if os.path.exists(mine):
                    os.remove(mine)
                continue
            a, b = chunks(wheel), chunks(mine)
            if len(a) != len(b) or any(len(x) != len(y) for x, y in zip(a, b)):
                failures.append(f"{layout} {comp}: chunk counts differ")
                continue
            i23 = comp in ("B44_COMPRESSION", "B44A_COMPRESSION") and not has_half_channel(src)
            for p, (x, y) in enumerate(zip(a, b)):
                for i, (cx, cy) in enumerate(zip(x, y)):
                    compared += 1
                    if cx == cy:
                        continue
                    msg = f"{layout} {comp}: part {p} chunk {i} differs ({len(cx)} vs {len(cy)} bytes)"
                    (upstream_bugs if i23 else failures).append(msg + (" I-23 (no half channel)" if i23 else ""))
            os.remove(mine)
    for f in upstream_bugs:
        print("upstream bug, not accepted:", f)
    for f in failures:
        print(f)
    print(f"chunks compared: {compared}; upstream bugs (not accepted): {len(upstream_bugs)}; "
          f"other differences or failures: {len(failures)}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
