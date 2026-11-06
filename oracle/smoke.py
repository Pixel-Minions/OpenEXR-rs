# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Smoke check of the oracle: the official openexr==3.5.3 wheel reads and writes files here, and
pixels come back bit for bit where the format is lossless.

  uv run --project oracle python -I oracle/smoke.py <out-dir> [<image> ...]

For every compression the wheel offers, on a scanline and a tiled part, it writes three channels
(every half bit pattern; random float and uint bit patterns from a fixed seed), reads them back
and compares the bits. Where the format is lossless for that pixel type (exr_compression_is_lossy
and the codecs' own rules, upstream src/lib/OpenEXRCore/compression.c:481-496 @ v3.5.3), a single
differing bit fails the check. It also writes each file twice and requires identical bytes, and
reports each file's SHA-256 so two machines can be compared. Every <image> is opened and its
parts listed (or the wheel's error reported).

This script reports what the library does; it computes no expected values.
"""

import hashlib
import json
import os
import platform
import sys

import numpy as np
import OpenEXR

W, H = 256, 256  # 65,536 pixels: one of each half bit pattern

# Pixel types each codec keeps exactly. Everything else is reported, not checked.
LOSSLESS = {
    "NO_COMPRESSION": {"half", "float", "uint"},
    "RLE_COMPRESSION": {"half", "float", "uint"},
    "ZIPS_COMPRESSION": {"half", "float", "uint"},
    "ZIP_COMPRESSION": {"half", "float", "uint"},
    "PIZ_COMPRESSION": {"half", "float", "uint"},
    "ZSTD_COMPRESSION": {"half", "float", "uint"},
    "HTJ2K256_COMPRESSION": {"half", "float", "uint"},
    "HTJ2K32_COMPRESSION": {"half", "float", "uint"},
    # PXR24 rounds float to 24 bits; half and uint pass exactly.
    "PXR24_COMPRESSION": {"half", "uint"},
    # B44 and B44A compress only half channels.
    "B44_COMPRESSION": {"float", "uint"},
    "B44A_COMPRESSION": {"float", "uint"},
    # DWA quantizes only the channels its classifier names (R, G, B, Y, BY, RY); the rest are
    # stored losslessly, which this check reports rather than relies on.
    "DWAA_COMPRESSION": {"uint"},
    "DWAB_COMPRESSION": {"uint"},
    "LJ2K_COMPRESSION": set(),
}


def inputs():
    rng = np.random.default_rng(20261106)
    half = np.arange(65536, dtype=np.uint16).view(np.float16).reshape(H, W)
    flt = rng.integers(0, 2**32, size=(H, W), dtype=np.uint32).view(np.float32)
    uint = rng.integers(0, 2**32, size=(H, W), dtype=np.uint32)
    return {"R": half, "Z": flt, "id": uint}


KIND = {"R": "half", "Z": "float", "id": "uint"}


def bits(a):
    return np.ascontiguousarray(a).view({2: np.uint16, 4: np.uint32}[a.dtype.itemsize])


def write(path, header, pixels):
    channels = {name: OpenEXR.Channel(name, arr, 1, 1) for name, arr in pixels.items()}
    with OpenEXR.File(header, channels) as f:
        f.write(path)
    with open(path, "rb") as fh:
        return fh.read()


def check(out_dir, storage, comp_name):
    comp = getattr(OpenEXR, comp_name)
    header = {"compression": comp}
    if storage == "tiled":
        tiles = OpenEXR.TileDescription()
        tiles.xSize, tiles.ySize = 64, 32
        header["type"] = OpenEXR.tiledimage
        header["tiles"] = tiles
    else:
        header["type"] = OpenEXR.scanlineimage
    pixels = inputs()
    path = os.path.join(out_dir, f"{storage}_{comp_name}.exr")
    case = {"storage": storage, "compression": comp_name}
    try:
        first = write(path, header, pixels)
        again = write(path + ".again", header, pixels)
    except Exception as exc:  # the wheel's refusal is the result
        case["write_error"] = f"{type(exc).__name__}: {exc}"
        return case, False
    os.remove(path + ".again")
    case["bytes"] = len(first)
    case["sha256"] = hashlib.sha256(first).hexdigest()
    case["deterministic"] = first == again
    ok = case["deterministic"]
    with OpenEXR.File(path, separate_channels=True) as f:
        got = f.channels()
        case["read_compression"] = str(f.parts[0].compression())
        for name, src in pixels.items():
            dst = got[name].pixels
            diff = int(np.count_nonzero(bits(src) != bits(dst))) if dst.dtype == src.dtype else -1
            exact_expected = KIND[name] in LOSSLESS[comp_name]
            case[f"{name}:{KIND[name]}"] = {
                "dtype": str(dst.dtype),
                "differing": diff,
                "checked": exact_expected,
                "decoded_sha256": hashlib.sha256(np.ascontiguousarray(dst).tobytes()).hexdigest(),
            }
            if exact_expected and diff != 0:
                ok = False
    return case, ok


def describe(image):
    try:
        with OpenEXR.File(image, separate_channels=True) as f:
            parts = []
            for p in f.parts:
                parts.append({
                    "name": p.name(), "type": str(p.type()), "compression": str(p.compression()),
                    "width": p.width(), "height": p.height(),
                    "channels": {n: str(c.pixels.dtype) for n, c in p.channels.items()},
                })
            return {"image": os.path.basename(image), "parts": parts}
    except Exception as exc:
        return {"image": os.path.basename(image), "error": f"{type(exc).__name__}: {exc}"}


def main():
    out_dir = sys.argv[1]
    os.makedirs(out_dir, exist_ok=True)
    report = {
        "openexr": OpenEXR.__version__,
        "numpy": np.__version__,
        "python": platform.python_version(),
        "platform": platform.platform(),
        "cases": [],
        "images": [describe(p) for p in sys.argv[2:]],
    }
    failures = 0
    for storage in ("scanline", "tiled"):
        for comp_name in LOSSLESS:
            case, ok = check(out_dir, storage, comp_name)
            case["ok"] = ok
            failures += not ok
            report["cases"].append(case)
    report["failures"] = failures
    with open(os.path.join(out_dir, "report.json"), "w") as fh:
        json.dump(report, fh, indent=1)
    for c in report["cases"]:
        detail = c.get("write_error") or " ".join(
            f"{k}={v['differing']}{'' if v['checked'] else '(lossy)'}"
            for k, v in c.items() if isinstance(v, dict))
        print(f"{'ok  ' if c['ok'] else 'FAIL'} {c['storage']:8} {c['compression']:22} "
              f"{c.get('sha256', '')[:16]} {detail}")
    print(f"images read: {sum('parts' in i for i in report['images'])}, "
          f"refused: {sum('error' in i for i in report['images'])}")
    print(f"{failures} failure(s); report in {os.path.join(out_dir, 'report.json')}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
