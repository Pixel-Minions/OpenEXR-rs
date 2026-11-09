# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every channel of every part of an image, as the official openexr==3.5.3 wheel reads it.

  uv run --project oracle python -I oracle/dump_pixels.py <image> <out-dir>
  uv run --project oracle python -I oracle/dump_pixels.py --many <list.txt> <out-dir>

The second form reads one image path per line of <list.txt> and dumps the n-th (from 0) to
<out-dir>/<n>, in one process.

The image is read with `separate_channels=True`. For channel number i of part p (the part's
number in the file; a part whose pixels the wheel cannot read is left out), <out-dir>/<p>.<i>.bin holds the channel's pixels in native byte order,
row by row, and <out-dir>/manifest.tsv gets one line, tab-separated:

  part  index  name (UTF-8, hex)  dtype  height  width  x_sampling  y_sampling  file

A deep channel's dtype is `deep-<sample dtype>`: <p>.<i>.counts holds its pixels' sample
counts (uint32, row by row) and <p>.<i>.bin its samples, pixel after pixel.

If the wheel refuses the image, manifest.tsv holds a single line `error<TAB><message>`.

The binding has no conversion for the standard `deepImageState` attribute and refuses any file
that holds one ("unsupported attribute type", PyOpenEXR.cpp:2058-2065), though the C++ library
reads it and never uses it to decode pixels. For such a file the wheel reads a copy whose
attribute has its type renamed to one of the same length the binding does not know
(`xeepImageState`, kept as an opaque attribute): every other byte, offsets included, is the
file's own.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

# a deep channel's samples, by the channel's type
DEEP_TYPES = {"PixelType.UINT": "uint32", "PixelType.HALF": "float16", "PixelType.FLOAT": "float32"}


def dump(image, out):
    os.makedirs(out, exist_ok=True)
    lines = []
    try:
        try:
            f = OpenEXR.File(image, separate_channels=True)
        except Exception as e:
            if "unsupported attribute type: deepImageState" not in str(e):
                raise
            data = open(image, "rb").read()
            renamed = os.path.join(out, "deepImageState-as-opaque.exr")
            with open(renamed, "wb") as fh:
                fh.write(data.replace(b"deepImageState\0deepImageState\0", b"deepImageState\0xeepImageState\0"))
            f = OpenEXR.File(renamed, separate_channels=True)
        # the part's number in the file: the binding leaves out a part whose pixels it cannot
        # read, so a part's place in f.parts is not its number
        for part in f.parts:
            p = part.part_index
            for i, (name, ch) in enumerate(part.channels.items()):
                px = ch.pixels
                fn = f"{p}.{i}.bin"
                h, w = px.shape[:2]
                if px.dtype == object:
                    dtype = "deep-" + DEEP_TYPES[str(ch.type())]
                    counts = np.zeros((h, w), dtype=np.uint32)
                    with open(os.path.join(out, fn), "wb") as fh:
                        for y in range(h):
                            for x in range(w):
                                s = px[y, x]
                                if s is not None:
                                    counts[y, x] = s.shape[0]
                                    fh.write(s.tobytes(order="C"))
                    with open(os.path.join(out, f"{p}.{i}.counts"), "wb") as fh:
                        fh.write(counts.tobytes(order="C"))
                else:
                    dtype = px.dtype.name
                    with open(os.path.join(out, fn), "wb") as fh:
                        fh.write(px.tobytes(order="C"))
                lines.append(
                    "\t".join(
                        str(v)
                        for v in (
                            p,
                            i,
                            name.encode("utf-8").hex(),
                            dtype,
                            h,
                            w,
                            ch.xSampling,
                            ch.ySampling,
                            fn,
                        )
                    )
                )
    except Exception as e:  # the wheel's refusal is the report
        lines = ["error\t" + " ".join(str(e).split())]
    with open(os.path.join(out, "manifest.tsv"), "w", encoding="utf-8", newline="\n") as fh:
        fh.write("\n".join(lines) + "\n")


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--many":
        images = [l for l in open(sys.argv[2], encoding="utf-8").read().split("\n") if l]
        for n, image in enumerate(images):
            dump(image, os.path.join(sys.argv[3], str(n)))
        return
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    dump(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    main()
