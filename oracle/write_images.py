# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Writes small images with the official openexr==3.5.3 wheel, in many channel layouts, for
the heavy-tier decode checks.

  uv run --project oracle python -I oracle/write_images.py <out-dir> <compression> ...

<compression> is a name of the wheel's (NO_COMPRESSION, RLE_COMPRESSION, ...). For each one
and each layout below it writes <out-dir>/<layout>_<compression>.exr: scanline layouts with 1
to 5 half channels, 3 and 4 float or uint channels, mixed types, x and y subsampling (on a
38 x 24 window, a whole number of samples); tiled layouts with 3 and 4 channels (one level);
a scanline and a tiled layout in decreasing-Y line order ("_dec"); a header holding an
attribute of every type the binding writes; and two large images whose pixels
compress ("smooth": gradients, flat areas, repeats and noisy bands, 2000 x 40), so that the
codecs' compressed paths are taken, not their fallback to the raw bytes. Pixel bits come from
a fixed seed. The images' pixels are what the wheel reads back (oracle/dump_pixels.py), not
what was written.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

W, H = 37, 23  # odd sizes: partial chunks, partial tiles
# a subsampled channel's data window must be a whole number of samples
SAMPLED_W, SAMPLED_H = 38, 24
# the "smooth" layouts: a ZIP chunk of 4 float channels is over 300 000 bytes, a ZIPS one
# over the 32 KiB deflate window
SMOOTH_W, SMOOTH_H = 2000, 40


def smooth(rng, w, h, phase):
    """Compressible values: a gradient, then a flat band, a band repeating a short row, and a
    noisy band, each a quarter of the rows."""
    y, x = np.mgrid[0:h, 0:w]
    v = np.sin(x / 97.0 + phase) * np.cos(y / 7.0) * 4.0
    q = h // 4
    v[q : 2 * q] = 1.5
    v[2 * q : 3 * q] = np.tile(np.arange(23) / 8.0, w // 23 + 1)[:w]
    v[3 * q :] += rng.normal(0.0, 0.01, size=(h - 3 * q, w))
    return v


def channel(rng, kind, w, h, phase=None):
    if phase is not None:
        v = smooth(rng, w, h, phase)
        if kind == "half":
            return v.astype(np.float16)
        if kind == "float":
            return v.astype(np.float32)
        return (np.abs(v) * 1000.0).astype(np.uint32)
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
    # decreasing-Y line order (the binding writes only level 0 of a mipmapped or ripmapped
    # part, so the tiled one has one level)
    "half3_dec": ("scanline", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)]),
    "tiled3_dec": ("tiled", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)]),
    "smooth_half4": ("scanline", [(c, "half", 1, 1) for c in "ABGR"]),
    "smooth_mixed": ("scanline", [("B", "half", 1, 1), ("G", "float", 1, 1), ("R", "float", 1, 1), ("id", "uint", 1, 1)]),
    # a header with an attribute of every type the binding writes (EXTRA_ATTRS)
    "attrs": ("scanline", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)]),
}


def extra_attrs():
    """The attributes of the "attrs" layout: every type the binding writes but rational."""
    return {
        "comments": "a comment",
        "blob": OpenEXR.Bytes(b"", "hint"),
        "emptyBlob": OpenEXR.Bytes(b"", ""),
        "noHint": OpenEXR.Bytes(b"", ""),
        "keyCode": OpenEXR.KeyCode(1, 2, 3, 4, 5, 4, 64),
        "floatList": [1.0, 2.5],
        "multiView": ["left", "right"],
        "v2iAttr": (1, 2),
        "v3fAttr": (1.0, 2.0, 3.0),
        "m44dAttr": np.eye(4),
        "m33fAttr": np.eye(3, dtype="float32"),
        "chromaticities": (0.64, 0.33, 0.3, 0.6, 0.15, 0.06, 0.3127, 0.329),
        "intAttr": 7,
        "floatAttr": 0.5,
        "preview": OpenEXR.PreviewImage(2, 2),
        "envmap": OpenEXR.ENVMAP_CUBE,
        "timeCode": OpenEXR.TimeCode(),
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
            if name.endswith("_dec"):
                header["lineOrder"] = OpenEXR.DECREASING_Y
            if name == "attrs":
                header.update(extra_attrs())
            sampled = any(xs > 1 or ys > 1 for _, _, xs, ys in chans)
            w, h = (SAMPLED_W, SAMPLED_H) if sampled else (W, H)
            is_smooth = name.startswith("smooth")
            if is_smooth:
                w, h = SMOOTH_W, SMOOTH_H
            channels = {}
            for i, (cname, kind, xs, ys) in enumerate(chans):
                # the wheel takes every channel at full size and keeps the samples it stores
                px = channel(rng, kind, w, h, float(i) if is_smooth else None)
                channels[cname] = OpenEXR.Channel(cname, px, xs, ys)
            path = os.path.join(out, f"{name}_{comp_name}.exr")
            with OpenEXR.File(header, channels) as f:
                f.write(path)


if __name__ == "__main__":
    main()
