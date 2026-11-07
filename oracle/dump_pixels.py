# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every channel of every part of an image, as the official openexr==3.5.3 wheel reads it.

  uv run --project oracle python -I oracle/dump_pixels.py <image> <out-dir>

The image is read with `separate_channels=True`. For channel number i of part p (in the order
the wheel lists them), <out-dir>/<p>.<i>.bin holds the channel's pixels in native byte order,
row by row, and <out-dir>/manifest.tsv gets one line, tab-separated:

  part  index  name (UTF-8, hex)  dtype  height  width  x_sampling  y_sampling  file

If the wheel refuses the image, manifest.tsv holds a single line `error<TAB><message>`.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import OpenEXR


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    image, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    lines = []
    try:
        f = OpenEXR.File(image, separate_channels=True)
        for p, part in enumerate(f.parts):
            for i, (name, ch) in enumerate(part.channels.items()):
                px = ch.pixels
                fn = f"{p}.{i}.bin"
                with open(os.path.join(out, fn), "wb") as fh:
                    fh.write(px.tobytes(order="C"))
                h, w = px.shape[:2]
                lines.append(
                    "\t".join(
                        str(v)
                        for v in (
                            p,
                            i,
                            name.encode("utf-8").hex(),
                            px.dtype.name,
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
