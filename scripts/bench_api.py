# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""The safe API's speed against the C++ library's (a measurement, never a check).

The files of scripts/bench.py (three corpus images rewritten by Core alone into each of the
14 compressions, in target/bench/; run it first, or build its files with its `prepare`). For
each file and each thread count (1, and the CPU's count): the wheel's (the C++ library's)
whole-file read, `OpenEXR.File(path, separate_channels=True, num_threads=n)` from a file in
memory's page cache (for one thread `num_threads=0`: the C++ library's 1 is a worker
thread beside the calling one), and its write, `File.write`, to a file; the API's read and write
(`Image::read` from memory, `Image::write_to` into memory: the openexr-rs example `api_bench`).
The best of <reps> runs. The wheel's times include building its numpy arrays; the API's,
its owned planes. Prints a table per compression and the totals over the images, and
writes target/bench/api_results.json.

Build first: `cargo build --release -p openexr-rs --example api_bench`.

usage: uv run --project oracle python -I scripts/bench_api.py [reps]
"""

import json
import os
import subprocess
import sys
import tempfile
import time

import OpenEXR

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WIN = os.name == "nt"
API = os.path.join(
    os.environ.get("CARGO_TARGET_DIR", os.path.join(ROOT, "target")),
    "release/examples/api_bench" + (".exe" if WIN else ""),
)
NAMES = ["NONE", "RLE", "ZIPS", "ZIP", "PIZ", "PXR24", "B44", "B44A", "DWAA", "DWAB",
         "HTJ2K256", "HTJ2K32", "LJ2K", "ZSTD"]
IMAGES = ["Blobbies", "Ocean", "singlepart.0001"]


def wheel(path, reps, threads):
    # the C++ library's 1 thread is a worker beside the calling thread, which overlaps
    # compression with the rest; 0 is the calling thread alone, the API's one thread
    n = 0 if threads == 1 else threads
    read = write = float("inf")
    f = None
    for _ in range(reps):
        t = time.perf_counter()
        f = OpenEXR.File(path, separate_channels=True, num_threads=n)
        read = min(read, time.perf_counter() - t)
    out = os.path.join(tempfile.gettempdir(), f"bench_api_{os.getpid()}.exr")
    for _ in range(reps):
        t = time.perf_counter()
        f.write(out)
        write = min(write, time.perf_counter() - t)
    os.remove(out)
    return read * 1e3, write * 1e3


def api(path, reps, threads):
    out = subprocess.run([API, path, str(reps), str(threads)], check=True,
                         capture_output=True, text=True).stdout.split()
    return float(out[1]), float(out[3])


def main():
    reps = int(sys.argv[1]) if len(sys.argv) > 1 else 5
    bench = os.path.join(ROOT, "target/bench")
    cpus = os.cpu_count() or 1
    OpenEXR.set_global_thread_count(cpus)
    results = {}
    for c, name in enumerate(NAMES):
        for img in IMAGES:
            path = os.path.join(bench, f"{img}.c{c}.exr")
            if not os.path.exists(path):
                sys.exit(f"{path} is missing: run scripts/bench.py first")
            for n in (1, cpus):
                r = {"wheel": wheel(path, reps, n), "api": api(path, reps, n)}
                results.setdefault(name, {}).setdefault(img, {})[n] = r
    with open(os.path.join(bench, "api_results.json"), "w", encoding="utf-8") as fh:
        json.dump(results, fh, indent=1)
    print(f"best of {reps}; ms summed over {', '.join(IMAGES)}; {cpus} threads in the right half")
    print(f"{'':10} {'read 1t':>16} {'write 1t':>16} {'read Nt':>16} {'write Nt':>16}")
    print(f"{'':10}" + f" {'C++':>7} {'API':>8}" * 4)
    for name in NAMES:
        row = f"{name:10}"
        for n in (1, cpus):
            for k in (0, 1):
                w = sum(results[name][i][n]["wheel"][k] for i in IMAGES)
                a = sum(results[name][i][n]["api"][k] for i in IMAGES)
                row += f" {w:7.1f} {a:8.1f}"
        print(row)


if __name__ == "__main__":
    main()
