# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Writes multipart images with the official openexr==3.5.3 wheel, uncompressed, for the heavy
tier's comparison of the multipart headers Core writes alone with Core's own (the source-built
harness, PLAN.md O1).

  uv run --project oracle python -I oracle/write_multipart.py <out-dir>

It writes <out-dir>/mp_<name>.exr for each layout below: parts of scanline and tiled storage
together, with different channel sets and types, subsampled channels, decreasing-Y line order,
and parts carrying attributes of their own (of every type the binding writes but bytes, which
Core cannot write: docs/improvements.md, I-14). Pixel bits come from a fixed seed, with
oracle/write_images.py's helpers.

This script reports what the library writes; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from write_images import SAMPLED_H, SAMPLED_W, H, W, channel, extra_attrs  # noqa: E402

# name -> [(part name, storage, line order, [(channel, kind, x sampling, y sampling)], extra)]
LAYOUTS = {
    "two": [
        ("beauty", "scanline", "inc", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)], None),
        ("depth", "scanline", "inc", [("Z", "float", 1, 1)], None),
    ],
    "scan_tile": [
        ("rgba", "scanline", "inc", [(c, "half", 1, 1) for c in "ABGR"], None),
        ("Z", "tiled", "inc", [("Z", "float", 1, 1)], None),
        ("id", "tiled", "inc", [("id", "uint", 1, 1)], None),
    ],
    "sampled": [
        ("luma", "scanline", "inc", [("Y", "half", 1, 1), ("BY", "half", 2, 2), ("RY", "half", 2, 2)], None),
        ("alpha", "scanline", "inc", [("A", "half", 1, 1)], None),
    ],
    "decreasing": [
        ("scan", "scanline", "dec", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)], None),
        ("tiles", "tiled", "dec", [("B", "half", 1, 1), ("G", "half", 1, 1), ("R", "half", 1, 1)], None),
    ],
    "attrs": [
        ("first", "scanline", "inc", [("Y", "half", 1, 1)], "first"),
        ("second", "tiled", "inc", [("Y", "half", 1, 1), ("id", "uint", 1, 1)], "second"),
        ("third", "scanline", "inc", [("Z", "float", 1, 1)], None),
    ],
}


def part_attrs(which):
    """Attributes of a part's own: every type of write_images.py's but bytes, some differing
    by part."""
    a = {k: v for k, v in extra_attrs().items() if not isinstance(v, OpenEXR.Bytes)}
    a["comments"] = f"the {which} part"
    a["intAttr"] = 7 if which == "first" else 9
    return a


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = sys.argv[1]
    os.makedirs(out, exist_ok=True)
    for name, parts in LAYOUTS.items():
        rng = np.random.default_rng(20261108)
        sampled = any(xs > 1 or ys > 1 for p in parts for _, _, xs, ys in p[3])
        w, h = (SAMPLED_W, SAMPLED_H) if sampled else (W, H)
        built = []
        for pname, storage, order, chans, extra in parts:
            header = {"compression": OpenEXR.NO_COMPRESSION}
            if storage == "tiled":
                tiles = OpenEXR.TileDescription()
                tiles.xSize, tiles.ySize = 16, 8
                header["type"] = OpenEXR.tiledimage
                header["tiles"] = tiles
            else:
                header["type"] = OpenEXR.scanlineimage
            if order == "dec":
                header["lineOrder"] = OpenEXR.DECREASING_Y
            if extra:
                header.update(part_attrs(extra))
            channels = {}
            for cname, kind, xs, ys in chans:
                channels[cname] = OpenEXR.Channel(cname, channel(rng, kind, w, h), xs, ys)
            built.append(OpenEXR.Part(header, channels, pname))
        with OpenEXR.File(built) as f:
            f.write(os.path.join(out, f"mp_{name}.exr"))


if __name__ == "__main__":
    main()
