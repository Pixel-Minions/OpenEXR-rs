# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""The source-built Core harness's acceptance for messages (PLAN.md O1): Core's own error text,
as the official openexr==3.5.3 wheel prints it, must be printed the same by the harness.

  uv run --project oracle python -I oracle/native_accept_messages.py <core_harness> [--damage <dir>] <file-or-dir> ...

With --damage, every image of the directories given (oracle/write_images.py's, for one) is
also checked damaged, in three copies written to <dir>: cut at three quarters of its length;
the 16 bytes from the middle of its first chunk set to 0xff; the first chunk's size set to
0x7fffffff. The damage is fixed by the file's layout, not drawn.

The wheel reads each file whole (every part's every channel), and Core, inside it, prints its
messages on stderr through its default error handler ("<file>: (<code>) <message>"). The
harness opens the same file as the C++ library does (silent header parse) and decodes every
chunk (`core_harness info --silent --no-print --read`). Every distinct message line of the
wheel's must be among the harness's, byte for byte. The harness may print more: the C++
library stops at the first chunk it refuses, where the harness goes on.

This script reports what the two printed; it computes no expected values.
"""

import os
import subprocess
import sys

READ = """
import sys, OpenEXR
try:
    with OpenEXR.File(sys.argv[1]) as f:
        for p in f.parts:
            for c in p.channels.values():
                c.pixels
except Exception:
    pass
"""


def core_lines(text, path):
    """The lines Core's default error handler printed for `path`."""
    prefix = path + ": (EXR_ERR_"
    return {line for line in text.splitlines() if line.startswith(prefix)}


def damaged(path, out):
    """The three damaged copies of `path` in `out`."""
    import struct

    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import native_accept

    data = open(path, "rb").read()
    try:
        parts, multipart, flags, table = native_accept.headers(data)
        first = struct.unpack_from("<Q", data, table)[0]
        lead = (4 if multipart else 0) + (16 if "tiles" in parts[0] else 4)
        if flags & 0x800:
            lead += 16  # deep: the table's and the data's packed sizes, before the unpacked
        size = struct.unpack_from("<i", data, first + lead)[0] if not flags & 0x800 else None
    except (ValueError, struct.error, IndexError, KeyError):
        return []
    base = os.path.join(out, os.path.basename(path)[:-4])
    copies = [(base + ".cut.exr", data[: len(data) * 3 // 4])]
    if size is not None and size > 32:
        mid = first + lead + 4 + size // 2
        copies.append((base + ".ff.exr", data[:mid] + bytes([0xFF]) * 16 + data[mid + 16:]))
        copies.append((base + ".size.exr", data[: first + lead] + struct.pack("<i", 0x7FFFFFFF) + data[first + lead + 4:]))
    for name, b in copies:
        open(name, "wb").write(b)
    return [name for name, _ in copies]


def files(args):
    for a in args:
        if os.path.isdir(a):
            for root, _, names in os.walk(a):
                for n in sorted(names):
                    if n.endswith(".exr"):
                        yield os.path.join(root, n)
        else:
            yield a


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    harness = os.path.abspath(sys.argv[1])
    args, damage = sys.argv[2:], None
    if args[:1] == ["--damage"]:
        damage, args = args[1], args[2:]
        os.makedirs(damage, exist_ok=True)
    paths = list(files(args))
    if damage:
        paths += [d for p in list(paths) for d in damaged(p, damage)]
    checked, with_messages, failures = 0, 0, []
    for path in paths:
        wheel = subprocess.run([sys.executable, "-I", "-c", READ, path], capture_output=True, text=True, errors="replace")
        mine = subprocess.run(
            [harness, "info", path, "--silent", "--no-print", "--read"], capture_output=True, text=True, errors="replace"
        )
        a, b = core_lines(wheel.stderr, path), core_lines(mine.stderr, path)
        checked += 1
        if a:
            with_messages += 1
        missing = sorted(a - b)
        for m in missing:
            failures.append(f"{path}: the harness does not print: {m}")
    for f in failures:
        print(f)
    print(f"files: {checked}, with wheel messages: {with_messages}, messages the harness misses: {len(failures)}")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
