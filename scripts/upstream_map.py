# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
"""Creates or extends upstream-map.toml: one entry per upstream file in the port's current scope
(PLAN.md §4), keeping every existing entry as it is.

  uv run --no-project python -I scripts/upstream_map.py

The scope is OpenEXRCore, its tests and the test images they open, and the three libraries
vendored into it (libdeflate, zstd, OpenJPH). The rest of the upstream tree is added when its
phase is planned. Until xtask exists, this script is the generator; it never changes the status,
target or note of an entry that is already there.
"""

import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UP = os.path.join(ROOT, "upstream", "openexr")
MAP = os.path.join(ROOT, "upstream-map.toml")

SCOPE = ("src/lib/OpenEXRCore/", "src/test/OpenEXRCoreTest/", "external/")
# Images of src/test/OpenEXRTest that OpenEXRCoreTest opens (read.cpp, ILM_IMF_TEST_IMAGEDIR).
CORE_TEST_IMAGES = {
    "invalid.exr", "tiled.exr", "v1.7.test.1.exr", "v1.7.test.interleaved.exr",
    "v1.7.test.planar.exr", "v1.7.test.tiled.exr",
}

ZSTD_COMPILED = {  # src/lib/OpenEXRCore/CMakeLists.txt:156-180 @ v3.5.3
    "common/debug.c", "common/entropy_common.c", "common/error_private.c",
    "common/fse_decompress.c", "common/pool.c", "common/threading.c", "common/xxhash.c",
    "common/zstd_common.c", "compress/fse_compress.c", "compress/hist.c",
    "compress/huf_compress.c", "compress/zstd_compress.c", "compress/zstd_compress_literals.c",
    "compress/zstd_compress_sequences.c", "compress/zstd_compress_superblock.c",
    "compress/zstd_double_fast.c", "compress/zstd_fast.c", "compress/zstd_lazy.c",
    "compress/zstd_ldm.c", "compress/zstd_opt.c", "compress/zstd_preSplit.c",
    "compress/zstdmt_compress.c", "decompress/huf_decompress.c", "decompress/zstd_ddict.c",
    "decompress/zstd_decompress.c", "decompress/zstd_decompress_block.c",
}
# libdeflate sources compression.c includes (src/lib/OpenEXRCore/compression.c:43-53 @ v3.5.3).
DEFLATE_UNUSED = {"lib/crc32.c", "lib/gzip_compress.c", "lib/gzip_decompress.c",
                  "lib/gzip_constants.h", "lib/crc32_multipliers.h", "lib/crc32_tables.h"}


def classify(path):
    """(kind, status, note) for a new entry."""
    name = os.path.basename(path)
    docs = {"README.md", "NEWS.md", "COPYING", "LICENSE", "current_deflate_version",
            "current_zstd_version", "patchup_deflate_lib.sed"}
    build = name == "CMakeLists.txt" or name.endswith((".cmake", ".cmake.in", ".pc.in"))
    if build:
        return "build", "n/a", "build system"
    if path.startswith("src/lib/OpenEXRCore/"):
        if name.startswith("openexr") and name.endswith(".h"):
            return "api", "todo", None
        return "src", "todo", None
    if path.startswith("src/test/OpenEXRCoreTest/"):
        if name in ("dwaLookups.h", "dwaQuantTables.h"):
            return "test-data", "todo", "generated tables the DWA tests compare against"
        if name == "performance.cpp":
            return "bench", "todo", "CorePerfTest; profiling comes later (owner, 2026-11-06)"
        return "test", "todo", None
    if path.startswith("src/test/OpenEXRTest/"):
        return "test-data", "todo", "opened by OpenEXRCoreTest (read.cpp)"
    if name in docs:
        if name in ("COPYING", "LICENSE"):
            return "license", "todo", "carried into THIRD-PARTY-NOTICES.md with the port"
        return "docs", "n/a", "documentation or vendoring metadata"
    if path.startswith("external/deflate/"):
        rel = path[len("external/deflate/"):]
        if rel in DEFLATE_UNUSED:
            return "vendored", "n/a", "not compiled into OpenEXRCore (compression.c:43-53)"
        if rel.startswith(("lib/arm/", "lib/riscv/")):
            return "vendored", "todo", "non-x86 CPU paths: only when ARM becomes a reference platform (D11)"
        return "vendored", "todo", None
    if path.startswith("external/zstd/"):
        rel = path[len("external/zstd/lib/"):]
        if path.endswith(".S"):
            return "vendored", "todo", "Linux x86-64 only (not MSVC); Huffman decode fast path, its output is format-defined"
        if path.endswith(".c") and rel not in ZSTD_COMPILED:
            return "vendored", "n/a", "not compiled into OpenEXRCore"
        return "vendored", "todo", None
    if path.startswith("external/OpenJPH/"):
        if re.search(r"_(vsx|wasm)\.(cpp|h)$", name) or name == "ojph_simd_vsx.h":
            return "vendored", "n/a", "not an x86-64 or ARM path"
        return "vendored", "todo", None
    return "other", "todo", None


def upstream_files():
    out = subprocess.run(["git", "-C", UP, "ls-files"], check=True, capture_output=True,
                         text=True).stdout.splitlines()
    files = [p for p in out if p.startswith(SCOPE)]
    files += [p for p in out if p.startswith("src/test/OpenEXRTest/")
              and os.path.basename(p) in CORE_TEST_IMAGES]
    return sorted(files)


def existing_paths(text):
    return set(re.findall(r'^path = "([^"]+)"$', text, flags=re.M))


def main():
    commit = subprocess.run(["git", "-C", UP, "rev-parse", "HEAD"], check=True,
                            capture_output=True, text=True).stdout.strip()
    tag = subprocess.run(["git", "-C", UP, "describe", "--tags", "--exact-match"], check=True,
                         capture_output=True, text=True).stdout.strip()
    if os.path.exists(MAP):
        with open(MAP, encoding="utf-8") as fh:
            text = fh.read()
    else:
        text = (
            "# Every upstream OpenEXR file the port accounts for (PLAN.md §7).\n"
            "# Created by scripts/upstream_map.py, which keeps existing entries; agents update\n"
            "# `target`, `status` and `note` as they port. status: todo | partial | done | n/a.\n"
            "# An n/a entry needs a note; n/a for anything other than build files, documentation,\n"
            "# sources upstream does not compile into the library, or platforms out of scope\n"
            "# needs the owner's approval in review.\n"
            "# Scope today: OpenEXRCore, its tests, and the libraries vendored into it (PLAN.md §4).\n\n"
            f'[upstream]\ntag = "{tag}"\ncommit = "{commit}"\n')
    if f'commit = "{commit}"' not in text:
        sys.exit(f"upstream-map.toml is for another upstream commit than {commit}")
    have = existing_paths(text)
    added = 0
    for path in upstream_files():
        if path in have:
            continue
        kind, status, note = classify(path)
        entry = f'\n[[file]]\npath = "{path}"\nkind = "{kind}"\nstatus = "{status}"\n'
        if note:
            entry += f'note = "{note}"\n'
        text += entry
        added += 1
    with open(MAP, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(text)
    print(f"{added} entries added; {len(have) + added} in total")


if __name__ == "__main__":
    main()
