# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Deep files written by the official openexr==3.5.3 wheel, standing in for the C++ generators
of upstream's deep tests (src/test/OpenEXRCoreTest/deep.cpp, generateRandomScanFile and
generateRandomTileFile @ v3.5.3).

  uv run --project oracle python -I oracle/write_deep.py <out.exr> <scan|tile> <channels> \\
      <compression> <seed>

As upstream's generators: display window (0, 0) - (556, 349), data window (10, 11) - (282,
179), increasing Y; channels named "0", "1", ... of a type drawn at random (UINT, HALF or
FLOAT); each pixel 1 to 10 samples, drawn at random, every sample of pixel (x, y) of the data
window holding (y * 273 + x) % 2049. <compression> is a name of the wheel's (NO_COMPRESSION,
...). A tiled file has a tile size drawn at random (1 to 273 by 1 to 169).

Where the stand-in differs from upstream's: the draws come from this script's generator,
seeded by <seed> (upstream's std::default_random_engine and uniform_int_distribution differ
between C++ libraries anyway), and a tiled file has one level, the only one the binding writes
(upstream's has RIP-map levels, of which its tests read level 0 only).

This script reports what the library does; it computes no expected values.
"""

import sys

import numpy as np
import OpenEXR

WIDTH, HEIGHT = 273, 169
MIN_X, MIN_Y = 10, 11
TYPES = [np.uint32, np.float16, np.float32]


class Draw:
    """A 64-bit linear congruential generator: draws in [0, range)."""

    def __init__(self, seed):
        self.x = seed & 0xFFFFFFFFFFFFFFFF

    def __call__(self, rng):
        self.x = (self.x * 6364136223846793005 + 1442695040888963407) & 0xFFFFFFFFFFFFFFFF
        return (self.x >> 33) % rng


def main():
    if len(sys.argv) != 6:
        sys.exit(__doc__)
    out, kind, nchan, comp_name, seed = sys.argv[1:6]
    nchan, draw = int(nchan), Draw(int(seed))
    tiled = kind == "tile"

    types = [TYPES[draw(3)] for _ in range(nchan)]
    header = {
        "compression": getattr(OpenEXR, comp_name),
        "type": OpenEXR.deeptile if tiled else OpenEXR.deepscanline,
        "displayWindow": ((0, 0), (MIN_X + WIDTH * 2, MIN_Y + HEIGHT * 2)),
        "dataWindow": ((MIN_X, MIN_Y), (MIN_X + WIDTH - 1, MIN_Y + HEIGHT - 1)),
        "lineOrder": OpenEXR.INCREASING_Y,
        "pixelAspectRatio": 1.0,
        "screenWindowWidth": 1.0,
    }
    if tiled:
        tiles = OpenEXR.TileDescription()
        tiles.xSize, tiles.ySize = draw(WIDTH) + 1, draw(HEIGHT) + 1
        tiles.mode = OpenEXR.ONE_LEVEL
        header["tiles"] = tiles

    channels = {str(i): np.empty((HEIGHT, WIDTH), dtype=object) for i in range(nchan)}
    for y in range(HEIGHT):
        for x in range(WIDTH):
            n = draw(10) + 1
            v = (y * WIDTH + x) % 2049
            for i, t in enumerate(types):
                channels[str(i)][y, x] = np.full(n, v, dtype=t)

    with OpenEXR.File(header, channels) as f:
        f.write(out)


if __name__ == "__main__":
    main()
