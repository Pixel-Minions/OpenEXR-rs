# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Sets the status, target and note of upstream-map.toml entries.

  uv run --no-project python -I scripts/map_set.py <path> <status> [<target>[,<target>...]] [--note <text>]

Only the named entry changes; `cargo xtask guards` checks the result.
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MAP = os.path.join(ROOT, "upstream-map.toml")


def main():
    args = sys.argv[1:]
    note = None
    if "--note" in args:
        i = args.index("--note")
        note = args[i + 1]
        del args[i:i + 2]
    if len(args) not in (2, 3):
        sys.exit(__doc__)
    path, status = args[0], args[1]
    target = args[2] if len(args) == 3 else None
    with open(MAP, encoding="utf-8") as fh:
        text = fh.read()
    blocks = text.split("\n[[file]]\n")
    for k, block in enumerate(blocks[1:], 1):
        if f'path = "{path}"\n' not in block:
            continue
        lines = [l for l in block.splitlines()
                 if not re.match(r"(status|target)\s*=", l) and not (note and l.startswith("note"))]
        out = []
        for l in lines:
            out.append(l)
            if l.startswith("kind"):
                out.append(f'status = "{status}"')
                if target:
                    out.append(f'target = "{target}"')
        if note:
            out.append(f'note = "{note}"')
        blocks[k] = "\n".join(out) + "\n"
        with open(MAP, "w", encoding="utf-8", newline="\n") as fh:
            fh.write("\n[[file]]\n".join(blocks))
        return
    sys.exit(f"no entry for {path}")


if __name__ == "__main__":
    main()
