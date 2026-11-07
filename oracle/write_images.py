# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Writes small images with the official openexr==3.5.3 wheel, in many channel layouts, for
the heavy-tier decode checks.

  uv run --project oracle python -I oracle/write_images.py <out-dir> <compression> ...

<compression> is a name of the wheel's (NO_COMPRESSION, RLE_COMPRESSION, ...). For each one
and each layout below it writes <out-dir>/<layout>_<compression>.exr: scanline layouts with 1
to 5 half channels, 3 and 4 float or uint channels, mixed types, x and y subsampling (on a
38 x 24 window, a whole number of samples); tiled layouts with 3 and 4 channels. Pixel bits
come from a fixed seed. The images' pixels are what the wheel reads back
(oracle/dump_pixels.py), not what was written.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

W, H = 37, 23  # odd sizes: partial chunks, partial tiles
# a subsampled channel's data window must be a whole number of samples
SAMPLED_W, SAMPLED_H = 38, 24


def channel(rng, kind, w, h):
    if kind == "half":
        return rng.integers(0, 2**16, size=(h, w), dtype=np.uint16).view(np.float16)
    bits = rng.integers(0, 2**32, size=(h, w), dtype=np.uint32)
    return bits.view(np.float32) if kind == "float" else bits


# name -> (storage, [(channel, kind, x sampling, y sampling)])
LAYOUTS = {
    "half1": ("scanline", [("Y", "half", 1, 1)]),
    "half2": ("scanline", [("A", "half", 1, 1), ("Y", "half", 1, 1)]),
    "half3": ("scanline", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)]),
    "half4": ("scanline", [(c, "half", 1, 1) for c in "ABGR"]),
    "half5": ("scanline", [(c, "half", 1, 1) for c in "ABGRZ"]),
    "float3": ("scanline", [(c, "float", 1, 1) for c in "BGR"]),
    "uint4": ("scanline", [(c, "uint", 1, 1) for c in "ABGR"]),
    "mixed": ("scanline", [("B", "half", 1, 1), ("G", "float", 1, 1), ("id", "uint", 1, 1)]),
    "sampled": ("scanline", [("Y", "half", 1, 1), ("BY", "half", 2, 2), ("RY", "half", 2, 2)]),
    "tiled3": ("tiled", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)]),
    "tiled4": ("tiled", [(c, "float", 1, 1) for c in "ABGR"]),
}


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for comp_name in sys.argv[2:]:
        comp = getattr(OpenEXR, comp_name)
        for name, (storage, chans) in LAYOUTS.items():
            rng = np.random.default_rng(20261107)
            header = {"compression": comp}
            if storage == "tiled":
                tiles = OpenEXR.TileDescription()
                tiles.xSize, tiles.ySize = 16, 8
                header["type"] = OpenEXR.tiledimage
                header["tiles"] = tiles
            else:
                header["type"] = OpenEXR.scanlineimage
            sampled = any(xs > 1 or ys > 1 for _, _, xs, ys in chans)
            w, h = (SAMPLED_W, SAMPLED_H) if sampled else (W, H)
            channels = {}
            for cname, kind, xs, ys in chans:
                # the wheel takes every channel at full size and keeps the samples it stores
                px = channel(rng, kind, w, h)
                channels[cname] = OpenEXR.Channel(cname, px, xs, ys)
            path = os.path.join(out, f"{name}_{comp_name}.exr")
            with OpenEXR.File(header, channels) as f:
                f.write(path)


if __name__ == "__main__":
    main()
