# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every part's header, as the official openexr==3.5.3 wheel's `OpenEXR.File` reads it.

  uv run --project oracle python -I oracle/dump_headers.py <image> <out.tsv>

<out.tsv> gets one line per attribute of each part, tab-separated:

  part  name (UTF-8, hex)  kind  value

or a single line `error<TAB><message>` if the wheel refuses the image. Each value is the
binding's Python object, written losslessly as text, by its kind:

  bytes      <hint hex>:<data hex>                           an `OpenEXR.Bytes`
  channels   <name hex>,<type>,<xs>,<ys>,<pLinear> ;-joined   a list of `Channel`s
  strs       <hex>,-joined                                    a list of `str` (UTF-8)
  floats     <bits>,-joined                                   a list (or tuple) of `float`s
  ints       <int>,-joined                                    a tuple of `int`s
  box        <dtype>:<min x>,<min y>:<max x>,<max y>          a tuple of two arrays
  array      <dtype>:<shape, x-joined>:<hex of its bytes>     a numpy array
  float      <bits>                                           a `float`, its IEEE double bits as hex
  int        <int>
  str        <hex>                                            its UTF-8 bytes
  enum       <type>:<int>                                     the binding's enums
  tiles      <xSize>,<ySize>,<mode>,<roundingMode>
  preview    <height>,<width>:<hex>                           RGBA bytes, row by row
  timecode   hours,minutes,seconds,frame,dropFrame,colorFrame,fieldPhase,bgf0,bgf1,bgf2,userData
  keycode    filmMfcCode,filmType,prefix,count,perfOffset,perfsPerFrame,perfsPerCount
  repr       <type>:<repr>                                    anything else

Strings are hex so that no decoding step stands between the file's bytes and the comparison;
floats are their bit patterns. The binding folds some types (a float and a double attribute
are both a Python `float`); those are checked against Core itself, not here. A part's channel
types come from its channels as the binding reads them with their pixels (a header's own
`Channel` objects cannot give theirs).

The binding refuses a file holding a `deepImageState` attribute (PyOpenEXR.cpp:2058-2065), as
`dump_pixels.py` notes; such a file is an error here.

This script reports what the library does; it computes no expected values.
"""

import struct
import sys

import numpy as np
import OpenEXR


def hexs(s):
    return s.encode("utf-8", "surrogateescape").hex()


def f64(x):
    return struct.pack(">d", float(x)).hex()


def canon(v, channels):
    """(kind, value text) of an attribute's Python object."""
    if isinstance(v, OpenEXR.Bytes):
        return "bytes", f"{hexs(v.type_hint)}:{bytes(v.data).hex()}"
    if isinstance(v, OpenEXR.TileDescription):
        return "tiles", f"{int(v.xSize)},{int(v.ySize)},{int(v.mode)},{int(v.roundingMode)}"
    if isinstance(v, OpenEXR.PreviewImage):
        px = np.ascontiguousarray(v.pixels)
        return "preview", f"{px.shape[0]},{px.shape[1]}:{px.tobytes().hex()}"
    if isinstance(v, OpenEXR.TimeCode):
        return "timecode", ",".join(str(int(getattr(v, a))) for a in (
            "hours", "minutes", "seconds", "frame", "dropFrame", "colorFrame", "fieldPhase",
            "bgf0", "bgf1", "bgf2", "userData"))
    if isinstance(v, OpenEXR.KeyCode):
        return "keycode", ",".join(str(int(getattr(v, a))) for a in (
            "filmMfcCode", "filmType", "prefix", "count", "perfOffset", "perfsPerFrame",
            "perfsPerCount"))
    if isinstance(v, list) and v and all(isinstance(c, OpenEXR.Channel) for c in v):
        return "channels", ";".join(
            f"{hexs(c.name)},{int(channels[c.name].type())},{int(c.xSampling)},"
            f"{int(c.ySampling)},{int(bool(c.pLinear))}" for c in v)
    if isinstance(v, list) and all(isinstance(x, str) for x in v):
        return "strs", ",".join(hexs(x) for x in v)
    if isinstance(v, (list, tuple)) and v and all(isinstance(x, float) for x in v):
        return "floats", ",".join(f64(x) for x in v)
    if isinstance(v, tuple) and v and all(isinstance(x, int) and not isinstance(x, bool) for x in v):
        return "ints", ",".join(str(x) for x in v)
    if isinstance(v, tuple) and len(v) == 2 and all(isinstance(x, np.ndarray) for x in v):
        mn, mx = v
        return "box", f"{mn.dtype}:{','.join(map(str, mn.tolist()))}:{','.join(map(str, mx.tolist()))}"
    if isinstance(v, np.ndarray):
        a = np.ascontiguousarray(v)
        return "array", f"{a.dtype}:{'x'.join(map(str, a.shape))}:{a.tobytes().hex()}"
    if isinstance(v, bool):
        return "int", str(int(v))
    if isinstance(v, float):
        return "float", f64(v)
    if isinstance(v, int):
        return "int", str(v)
    if isinstance(v, str):
        return "str", hexs(v)
    if hasattr(v, "__int__") and type(v).__module__ == "OpenEXR":
        return "enum", f"{type(v).__name__}:{int(v)}"
    return "repr", f"{type(v).__name__}:{repr(v)}".replace("\t", " ").replace("\n", " ")


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    image, out = sys.argv[1], sys.argv[2]
    lines = []
    try:
        f = OpenEXR.File(image, separate_channels=True)
        for p in f.parts:
            for name, v in p.header.items():
                kind, text = canon(v, p.channels)
                lines.append(f"{int(p.part_index)}\t{hexs(name)}\t{kind}\t{text}")
    except Exception as e:  # the wheel's refusal, reported as it gives it
        lines = ["error\t" + str(e).replace("\t", " ").replace("\n", " ")]
    with open(out, "w", encoding="utf-8", newline="\n") as fp:
        fp.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
