# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""The port's speed against Core's (a measurement, never a check).

Three corpus images, each rewritten by Core alone (core_harness recompress) into each of the
14 compressions, into target/bench/; then, for each file, Core's times (core_harness bench)
and the port's (the openexr-rs-core example `bench`, the same work), alternating which runs
first, the best of <reps> runs over <rounds> rounds. Prints a table per compression, the
times summed over the images, and writes target/bench/results.json.

Build first: scripts/oracle-native-build (the harness) and
`cargo build --release -p openexr-rs-core --example bench`. Single-threaded on both sides;
decode times include reading the file, the same for both.

usage: uv run --no-project python -I scripts/bench.py [reps] [rounds]
"""

import json
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WIN = os.name == "nt"
HARNESS = os.path.join(
    ROOT,
    "target/oracle-native/msvc-Release/core_harness.exe"
    if WIN
    else "target/rocky9/oracle-native/gcc-Release/core_harness",
)
PORT = os.path.join(
    os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target")),
    "release/examples/bench" + (".exe" if WIN else ""),
)
NAMES = ["NONE", "RLE", "ZIPS", "ZIP", "PIZ", "PXR24", "B44", "B44A", "DWAA", "DWAB",
         "HTJ2K256", "HTJ2K32", "LJ2K", "ZSTD"]
SOURCES = {
    "Blobbies": "ScanLines/Blobbies.exr",  # scanlines, 5 channels
    "Ocean": "Tiles/Ocean.exr",  # tiles
    "singlepart.0001": "Beachball/singlepart.0001.exr",  # 20 channels
}


def prepare(bench):
    os.makedirs(bench, exist_ok=True)
    for name, rel in SOURCES.items():
        src = os.path.join(ROOT, "corpus/openexr-images", rel)
        for c in range(len(NAMES)):
            out = os.path.join(bench, f"{name}.c{c}.exr")
            if not os.path.exists(out):
                subprocess.run([HARNESS, "recompress", src, out, str(c)], check=True)


def run(cmd):
    res = {}
    for line in subprocess.run(cmd, capture_output=True, text=True, check=True).stdout.split("\n"):
        if line:
            k, _, t = line.split()
            res[k] = float(t)
    return res


def main():
    reps = sys.argv[1] if len(sys.argv) > 1 else "7"
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    bench = os.path.join(ROOT, "target/bench")
    prepare(bench)
    best = {}
    for rnd in range(rounds):
        for c in range(len(NAMES)):
            for s in SOURCES:
                f = os.path.join(bench, f"{s}.c{c}.exr")
                sides = [("core", [HARNESS, "bench", f, reps, str(c)]), ("port", [PORT, f, reps, str(c)])]
                if (rnd + c) % 2:
                    sides.reverse()
                for who, cmd in sides:
                    for k, t in run(cmd).items():
                        key = (c, s, who, k)
                        best[key] = min(best.get(key, 1e30), t)
    rows = []
    print(f"{'compression':<10} {'decode Core':>12} {'port':>9} {'ratio':>6}   "
          f"{'encode Core':>12} {'port':>9} {'ratio':>6}")
    for c, name in enumerate(NAMES):
        t = {(w, k): sum(best[(c, s, w, k)] for s in SOURCES) * 1e3
             for w in ("core", "port") for k in ("decode", "encode")}
        rows.append({"compression": name, **{f"{k}_{w}_ms": v for (w, k), v in t.items()}})
        print(f"{name:<10} {t['core', 'decode']:>10.2f}ms {t['port', 'decode']:>7.2f}ms "
              f"{t['port', 'decode'] / t['core', 'decode']:>6.2f}   {t['core', 'encode']:>10.2f}ms "
              f"{t['port', 'encode']:>7.2f}ms {t['port', 'encode'] / t['core', 'encode']:>6.2f}")
    with open(os.path.join(bench, "results.json"), "w", encoding="utf-8") as fp:
        json.dump(rows, fp, indent=1)


if __name__ == "__main__":
    main()
