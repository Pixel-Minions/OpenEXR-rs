# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Dumps every part's header, as the official openexr==3.5.3 wheel's `OpenEXR.File` reads it.

  uv run --project oracle python -I oracle/dump_headers.py <image> <out.tsv>
  uv run --project oracle python -I oracle/dump_headers.py --many <list.txt> <out dir>

The second form reads one image path per line of <list.txt> and writes the n-th (from 0) to
<out dir>/<n>.tsv, in one process. Each <out.tsv> gets one line per attribute of each part, tab-separated:

  part  name (UTF-8, hex)  kind  value

or a single line `error<TAB><message>` if the wheel refuses the image. Each value is the
binding's Python object, written losslessly as text, by its kind:

  bytes      <hint hex>:<data hex>                           an `OpenEXR.Bytes`
  channels   <name hex>,<type or ->,<xs>,<ys>,<pLinear> ;-joined   a list of `Channel`s
  strs       <hex>,-joined                                    a list of `str` (UTF-8)
  floats     <bits>,-joined                                   a list (or tuple) of `float`s
  ints       <int>,-joined                                    a tuple of `int`s
  box        <dtype>:<min hex>:<max hex>                      a tuple of two arrays, their bytes
  array      <dtype>:<shape, x-joined>:<hex of its bytes>     a numpy array
  float      <bits>                                           a `float`, its IEEE double bits as hex
  int        <int>
  str        <hex>                                            its UTF-8 bytes
  enum       <type>:<int>                                     the binding's enums
  tiles      <xSize>,<ySize>,<mode>,<roundingMode>
  preview    <height>,<width>:<hex>                           RGBA bytes, row by row
  timecode   hours,minutes,seconds,frame,dropFrame,colorFrame,fieldPhase,bgf0,bgf1,bgf2,userData
  keycode    filmMfcCode,filmType,prefix,count,perfOffset,perfsPerFrame,perfsPerCount
  opaque     <type name hex>:<data hex>                       an `OpenEXR.OpaqueAttribute`
  rational   <numerator>,<denominator>                        a `fractions.Fraction`
  repr      <type>:<repr>                                    anything else

Strings are hex so that no decoding step stands between the file's bytes and the comparison;
floats are their bit patterns. The binding folds some types (a float and a double attribute
are both a Python `float`); those are checked against Core itself, not here.

The headers come from `OpenEXR.File(image, header_only=True)`, which gives every part: a
pixel read drops each part whose pixels the wheel cannot read (PyOpenEXR.cpp:445-466 @
v3.5.3). A header's `Channel` objects carry no pixel type (PyOpenEXR.cpp:1842-1855), so each
channel's type comes from a second open that reads the pixels, as the type of the channel the
binding reads; a channel of a part whose pixels the wheel cannot read has type `-`.

The binding refuses a file holding an attribute type it does not know, `deepImageState`
among them (PyOpenEXR.cpp:2064-2066), as `dump_pixels.py` notes; such a file is an error here.

This script reports what the library does; it computes no expected values.
"""

import fractions
import os
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
    if isinstance(v, OpenEXR.OpaqueAttribute):
        return "opaque", f"{hexs(v.typeName)}:{bytes(v.data).hex()}"
    if isinstance(v, fractions.Fraction):
        return "rational", f"{v.numerator},{v.denominator}"
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
            f"{hexs(c.name)},{channels.get(c.name, '-')},{int(c.xSampling)},"
            f"{int(c.ySampling)},{int(bool(c.pLinear))}" for c in v)
    if isinstance(v, list) and all(isinstance(x, str) for x in v):
        return "strs", ",".join(hexs(x) for x in v)
    if isinstance(v, (list, tuple)) and v and all(isinstance(x, float) for x in v):
        return "floats", ",".join(f64(x) for x in v)
    if isinstance(v, tuple) and v and all(isinstance(x, int) and not isinstance(x, bool) for x in v):
        return "ints", ",".join(str(x) for x in v)
    if isinstance(v, tuple) and len(v) == 2 and all(isinstance(x, np.ndarray) for x in v):
        mn, mx = v
        return "box", (f"{mn.dtype}:{np.ascontiguousarray(mn).tobytes().hex()}:"
                       f"{np.ascontiguousarray(mx).tobytes().hex()}")
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


def channel_types(image):
    """{part index: {channel name: type}} of the parts whose pixels the wheel reads."""
    try:
        f = OpenEXR.File(image, separate_channels=True)
    except Exception:
        return {}
    return {int(p.part_index): {n: int(c.type()) for n, c in p.channels.items()}
            for p in f.parts}


def dump(image, out):
    lines = []
    try:
        f = OpenEXR.File(image, header_only=True)
        types = channel_types(image)
        for p in f.parts:
            for name, v in p.header.items():
                kind, text = canon(v, types.get(int(p.part_index), {}))
                lines.append(f"{int(p.part_index)}\t{hexs(name)}\t{kind}\t{text}")
    except Exception as e:  # the wheel's refusal, reported as it gives it
        lines = ["error\t" + str(e).replace("\t", " ").replace("\n", " ")]
    with open(out, "w", encoding="utf-8", newline="\n") as fp:
        fp.write("\n".join(lines) + "\n")


def main():
    if len(sys.argv) == 4 and sys.argv[1] == "--many":
        images = [l for l in open(sys.argv[2], encoding="utf-8").read().split("\n") if l]
        for n, image in enumerate(images):
            dump(image, os.path.join(sys.argv[3], f"{n}.tsv"))
        return
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    dump(sys.argv[1], sys.argv[2])


if __name__ == "__main__":
    main()
