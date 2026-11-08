# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every channel of every part of an image, as the official openexr==3.5.3 wheel reads it.

  uv run --project oracle python -I oracle/dump_pixels.py <image> <out-dir>

The image is read with `separate_channels=True`. For channel number i of part p (the part's
number in the file; a part whose pixels the wheel cannot read is left out), <out-dir>/<p>.<i>.bin holds the channel's pixels in native byte order,
row by row, and <out-dir>/manifest.tsv gets one line, tab-separated:

  part  index  name (UTF-8, hex)  dtype  height  width  x_sampling  y_sampling  file

A deep channel's dtype is `deep-<sample dtype>`: <p>.<i>.counts holds its pixels' sample
counts (uint32, row by row) and <p>.<i>.bin its samples, pixel after pixel.

If the wheel refuses the image, manifest.tsv holds a single line `error<TAB><message>`.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

# a deep channel's samples, by the channel's type
DEEP_TYPES = {"PixelType.UINT": "uint32", "PixelType.HALF": "float16", "PixelType.FLOAT": "float32"}


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    image, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    lines = []
    try:
        f = OpenEXR.File(image, separate_channels=True)
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


if __name__ == "__main__":
    main()
