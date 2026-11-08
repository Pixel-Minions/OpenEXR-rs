#!/usr/bin/env bash
# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# Build the source-built Core harness (PLAN.md O1, oracle/native) out of tree, with the
# settings of scripts/upstream-build.sh. On Rocky Linux 9: scripts/rocky9.sh scripts/oracle-native-build.sh
set -euo pipefail
config="${1:-Release}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
build="${OPENEXR_RS_ORACLE_NATIVE_BUILD:-$root/target/rocky9/oracle-native/gcc-$config}"
cmake=(uvx --from cmake==4.4.4 cmake)
mkdir -p "$build"
ninja="$(uvx --from ninja==1.13.0 python -c 'import ninja,os;print(os.path.join(ninja.BIN_DIR,"ninja"))')"

"${cmake[@]}" -S "$root/oracle/native" -B "$build" -G Ninja \
  -DCMAKE_MAKE_PROGRAM="$ninja" \
  -DCMAKE_BUILD_TYPE="$config"
"${cmake[@]}" --build "$build" --target core_harness
