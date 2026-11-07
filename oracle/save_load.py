# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""The C++ half of upstream's compression tests (src/test/OpenEXRCoreTest/compression.cpp,
saveCPP and loadCPP @ v3.5.3), done by the official openexr==3.5.3 wheel.

  uv run --project oracle python -I oracle/save_load.py <dir> <cpp.exr> <core.exr> \\
      <tiled 0|1> <xs> <ys> <compression> <fw> <fh> <dwx> <dwy>

<dir> holds the test's pixels, one file per channel, each sampled channel's values in raster
order and native byte order: I.bin (uint32), R.bin, G.bin, B.bin, A.bin and H.bin (half bits)
and F.bin (float32), each (fh / ys) x (fw / xs). The script writes them to <cpp.exr> as
saveCPP does: display window (0, 0) - (fw - 1, fh - 1), data window from (dwx, dwy) of the
same size, every channel sampled xs, ys, 32 x 32 one-level tiles when tiled, and the given
<compression> (a name of the wheel's, such as ZIP_COMPRESSION). saveCPP sets the zip level to
4, which the binding has no setting for: 4 is the library's default.

Then, as loadCPP, it reads <cpp.exr> and <core.exr> back and writes each channel's pixels to
<dir>/cpp.<name>.bin and <dir>/core.<name>.bin, as the wheel returns them (native byte order,
row by row).

The wheel stores a subsampled channel as the first (h / ys) x (w / xs) values of the array it
is given, in raster order, so each channel is handed over full size with its samples first.

This script reports what the library does; it computes no expected values.
"""

import os
import sys

import numpy as np
import OpenEXR

CHANNELS = {
    "I": np.uint32,
    "R": np.float16,
    "G": np.float16,
    "B": np.float16,
    "A": np.float16,
    "H": np.float16,
    "F": np.float32,
}


def main():
    if len(sys.argv) != 12:
        sys.exit(__doc__)
    d, cpp, core = sys.argv[1], sys.argv[2], sys.argv[3]
    tiled = sys.argv[4] == "1"
    xs, ys = int(sys.argv[5]), int(sys.argv[6])
    comp = getattr(OpenEXR, sys.argv[7])
    fw, fh, dwx, dwy = (int(v) for v in sys.argv[8:12])
    sw, sh = fw // xs, fh // ys

    header = {
        "compression": comp,
        "displayWindow": ((0, 0), (fw - 1, fh - 1)),
        "dataWindow": ((dwx, dwy), (dwx + fw - 1, dwy + fh - 1)),
    }
    if tiled:
        tiles = OpenEXR.TileDescription()
        tiles.xSize, tiles.ySize = 32, 32
        header["type"] = OpenEXR.tiledimage
        header["tiles"] = tiles
    else:
        header["type"] = OpenEXR.scanlineimage

    channels = {}
    for name, dtype in CHANNELS.items():
        raw = np.fromfile(os.path.join(d, f"{name}.bin"), dtype=dtype)
        full = np.zeros(fw * fh, dtype=dtype)
        full[: sw * sh] = raw[: sw * sh]
        channels[name] = OpenEXR.Channel(name, full.reshape(fh, fw), xs, ys)
    with OpenEXR.File(header, channels) as f:
        f.write(cpp)

    for which, path in (("cpp", cpp), ("core", core)):
        with OpenEXR.File(path, separate_channels=True) as f:
            for name, ch in f.parts[0].channels.items():
                with open(os.path.join(d, f"{which}.{name}.bin"), "wb") as fh_:
                    fh_.write(ch.pixels.tobytes(order="C"))


if __name__ == "__main__":
    main()
