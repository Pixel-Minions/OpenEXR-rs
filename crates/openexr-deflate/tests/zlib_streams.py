# SPDX-License-Identifier: MIT
# Copyright 2016 Eric Biggers
"""Writes DEFLATE and zlib streams made by the platform's zlib (Python's zlib module), for
tests/heavy_zlib_streams.rs.

  uv run --no-project python -I zlib_streams.py <out-dir>

For each payload (sizes from 0 to 300,000 bytes; random, text-like, runs, zeros) it writes
<p>.in, and for each level (0, 1, 6, 9) and strategy (default, filtered, Huffman only, RLE,
fixed) the zlib stream <p>.<level>.<strategy>.zlib and the raw stream ...raw. manifest.tsv
lists each payload with zlib's Adler-32 of it.
"""

import os
import random
import sys
import zlib

SIZES = [0, 1, 100, 5000, 70000, 300000]
LEVELS = [0, 1, 6, 9]
STRATEGIES = {
    "default": zlib.Z_DEFAULT_STRATEGY,
    "filtered": zlib.Z_FILTERED,
    "huffman": zlib.Z_HUFFMAN_ONLY,
    "rle": zlib.Z_RLE,
    "fixed": zlib.Z_FIXED,
}


def payload(kind, n, rng):
    if kind == "random":
        return bytes(rng.getrandbits(8) for _ in range(n))
    if kind == "text":
        words = [b"open", b"exr", b"half", b"pixel", b"chunk", b"line", b"tile", b"zip"]
        out = bytearray()
        while len(out) < n:
            out += rng.choice(words) + b" "
        return bytes(out[:n])
    if kind == "runs":
        out = bytearray()
        while len(out) < n:
            out += bytes([rng.getrandbits(8)]) * rng.randint(1, 300)
        return bytes(out[:n])
    return bytes(n)


def main():
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    rng = random.Random(20261107)
    lines = []
    for kind in ["random", "text", "runs", "zeros"]:
        for n in SIZES:
            name = f"{kind}{n}"
            data = payload(kind, n, rng)
            with open(os.path.join(out, name + ".in"), "wb") as f:
                f.write(data)
            lines.append(f"{name}\t{zlib.adler32(data)}")
            for level in LEVELS:
                for sname, strategy in STRATEGIES.items():
                    for wbits, ext in [(15, "zlib"), (-15, "raw")]:
                        c = zlib.compressobj(level, zlib.DEFLATED, wbits, 9, strategy)
                        stream = c.compress(data) + c.flush()
                        with open(os.path.join(out, f"{name}.{level}.{sname}.{ext}"), "wb") as f:
                            f.write(stream)
    with open(os.path.join(out, "manifest.tsv"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
