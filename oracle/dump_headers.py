# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every part's header, as the official openexr==3.5.3 wheel's `OpenEXR.File` reads it.

  uv run --project oracle python -I oracle/dump_headers.py <image> <out.json>

<out.json> holds {"parts": [{"index": p, "attrs": {name: value}}]} for each part (its number in
the file), or {"error": message} if the wheel refuses the image. Each value is the binding's
Python object, written losslessly as an object with a "t" (its kind) and its contents:

  bytes      {"t": "bytes", "hint": hex, "data": hex}          an `OpenEXR.Bytes`
  channels   {"t": "channels", "v": [{"name": hex, "type": int, "xs": int, "ys": int,
                                       "plinear": bool}]}        a list of `Channel`s
  strs       {"t": "strs", "v": [hex]}                           a list of `str`
  floats     {"t": "floats", "v": [hex]}                         a list (or tuple) of `float`
  box        {"t": "box", "dtype": str, "min": [..], "max": [..]} a tuple of two arrays
  array      {"t": "array", "dtype": str, "shape": [..], "hex": hex}   a numpy array (its bytes)
  float      {"t": "float", "v": hex}                            a `float`, its IEEE double bits
  int        {"t": "int", "v": int}
  str        {"t": "str", "v": hex}                              its UTF-8 bytes
  enum       {"t": "enum", "type": name, "v": int}               the binding's enums
  tiles      {"t": "tiles", "v": [xSize, ySize, mode, roundingMode]}
  preview    {"t": "preview", "shape": [h, w], "hex": hex}       RGBA bytes, row by row
  timecode   {"t": "timecode", "v": [hours, minutes, seconds, frame, dropFrame, colorFrame,
                                     fieldPhase, bgf0, bgf1, bgf2, userData]}
  keycode    {"t": "keycode", "v": [filmMfcCode, filmType, prefix, count, perfOffset,
                                    perfsPerFrame, perfsPerCount]}
  repr       {"t": "repr", "type": name, "v": repr}              anything else

Strings are hex so that no decoding step stands between the file's bytes and the comparison;
floats are their bit patterns. The binding folds some types (a float and a double attribute are
both a Python `float`); those are checked against Core itself, not here.

A part's channel types come from its channels as the binding reads them with their pixels (a
header's own `Channel` objects cannot give theirs).

The binding refuses a file holding a `deepImageState` attribute (PyOpenEXR.cpp:2058-2065), as
`dump_pixels.py` notes; such a file is an error here.

This script reports what the library does; it computes no expected values.
"""

import json
import struct
import sys

import numpy as np
import OpenEXR


def hexs(s):
    return s.encode("utf-8", "surrogateescape").hex()


def f64(x):
    return struct.pack("<d", float(x)).hex()


def canon(v, channels=None):
    if isinstance(v, OpenEXR.Bytes):
        return {"t": "bytes", "hint": hexs(v.type_hint), "data": bytes(v.data).hex()}
    if isinstance(v, OpenEXR.TileDescription):
        return {"t": "tiles", "v": [int(v.xSize), int(v.ySize), int(v.mode), int(v.roundingMode)]}
    if isinstance(v, OpenEXR.PreviewImage):
        px = np.ascontiguousarray(v.pixels)
        return {"t": "preview", "shape": list(px.shape), "hex": px.tobytes().hex()}
    if isinstance(v, OpenEXR.TimeCode):
        return {"t": "timecode", "v": [int(getattr(v, a)) for a in (
            "hours", "minutes", "seconds", "frame", "dropFrame", "colorFrame", "fieldPhase",
            "bgf0", "bgf1", "bgf2", "userData")]}
    if isinstance(v, OpenEXR.KeyCode):
        return {"t": "keycode", "v": [int(v.filmMfcCode), int(v.filmType), int(v.prefix),
                                      int(v.count), int(v.perfOffset), int(v.perfsPerFrame),
                                      int(v.perfsPerCount)]}
    if isinstance(v, list) and v and all(isinstance(c, OpenEXR.Channel) for c in v):
        # (a header's Channel derives its type from pixels it does not hold: the part's
        # channels, read with their pixels, give it)
        return {"t": "channels", "v": [{"name": hexs(c.name), "type": int(channels[c.name].type()),
                                        "xs": int(c.xSampling), "ys": int(c.ySampling),
                                        "plinear": bool(c.pLinear)} for c in v]}
    if isinstance(v, list) and all(isinstance(x, str) for x in v):
        return {"t": "strs", "v": [hexs(x) for x in v]}
    if isinstance(v, (list, tuple)) and v and all(isinstance(x, float) for x in v):
        return {"t": "floats", "v": [f64(x) for x in v]}
    if isinstance(v, tuple) and len(v) == 2 and all(isinstance(x, np.ndarray) for x in v):
        return {"t": "box", "dtype": str(v[0].dtype), "min": v[0].tolist(), "max": v[1].tolist()}
    if isinstance(v, np.ndarray):
        a = np.ascontiguousarray(v)
        return {"t": "array", "dtype": str(a.dtype), "shape": list(a.shape),
                "hex": a.tobytes().hex()}
    if isinstance(v, bool):
        return {"t": "int", "v": int(v)}
    if isinstance(v, float):
        return {"t": "float", "v": f64(v)}
    if isinstance(v, int):
        return {"t": "int", "v": v}
    if isinstance(v, str):
        return {"t": "str", "v": hexs(v)}
    if hasattr(v, "__int__") and type(v).__module__ == "OpenEXR":
        return {"t": "enum", "type": type(v).__name__, "v": int(v)}
    return {"t": "repr", "type": type(v).__name__, "v": repr(v)}


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    image, out = sys.argv[1], sys.argv[2]
    try:
        f = OpenEXR.File(image, separate_channels=True)
        parts = [{"index": int(p.part_index),
                  "attrs": {k: canon(v, p.channels) for k, v in p.header.items()}}
                 for p in f.parts]
        result = {"parts": parts}
    except Exception as e:  # the wheel's refusal, reported as it gives it
        result = {"error": str(e)}
    with open(out, "w", encoding="utf-8") as fp:
        json.dump(result, fp, indent=1, sort_keys=True)


if __name__ == "__main__":
    main()
