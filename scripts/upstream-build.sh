#!/usr/bin/env bash
# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# Linux twin of scripts/upstream-build.cmd: builds upstream OpenEXR (OpenEXRCore, OpenEXR,
# OpenEXRCoreTest) from the pinned submodule with the official wheel's dependency settings
# (upstream pyproject.toml), then runs the OpenEXRCore tests. Run it in the reference container:
#   scripts/rocky9.sh scripts/upstream-build.sh [Release|Debug] [ctest-args...]
set -euo pipefail
config="${1:-Release}"
shift || true
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build="${OPENEXR_RS_UPSTREAM_BUILD:-$root/target/rocky9/upstream-build/gcc-$config}"
cmake=(uvx --from cmake==4.4.4 cmake)
ctest=(uvx --from cmake==4.4.4 ctest)
mkdir -p "$build"
ninja="$(uvx --from ninja==1.13.0 python -c 'import ninja,os;print(os.path.join(ninja.BIN_DIR,"ninja"))')"

"${cmake[@]}" -S "$root/upstream/openexr" -B "$build" -G Ninja \
  -DCMAKE_MAKE_PROGRAM="$ninja" \
  -DCMAKE_BUILD_TYPE="$config" \
  -DBUILD_SHARED_LIBS=OFF \
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
  -DOPENEXR_INSTALL=OFF \
  -DOPENEXR_INSTALL_PKG_CONFIG=OFF \
  -DOPENEXR_BUILD_TOOLS=OFF \
  -DOPENEXR_BUILD_EXAMPLES=OFF \
  -DOPENEXR_BUILD_PYTHON=OFF \
  -DOPENEXR_FORCE_INTERNAL_DEFLATE=ON \
  -DOPENEXR_FORCE_INTERNAL_IMATH=ON \
  -DOPENEXR_FORCE_INTERNAL_OPENJPH=ON \
  -DOPENEXR_FORCE_INTERNAL_ZSTD=ON \
  -DBUILD_TESTING=ON \
  -DOPENEXR_TEST_LIBRARIES=ON \
  -DOPENEXR_TEST_TOOLS=OFF \
  -DOPENEXR_TEST_PYTHON=OFF

"${cmake[@]}" --build "$build" --target OpenEXRCoreTest
"${ctest[@]}" --test-dir "$build" -L OpenEXRCore --output-on-failure --timeout 1800 "$@"
