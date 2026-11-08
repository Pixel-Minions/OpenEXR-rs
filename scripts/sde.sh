#!/usr/bin/env bash
# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# Runs a command on an emulated x86 CPU with Intel SDE (PLAN.md §8, "Emulated CPUs"), so the
# official wheel, the source-built harness and the port all choose the SIMD paths that CPU
# gets. This machine's own CPU (AVX-512) only covers the paths it selects.
#   scripts/sde.sh snb cargo test -p openexr-rs-core --release --features heavy --test heavy_decode
#   scripts/rocky9.sh scripts/sde.sh hsw cargo test -p openexr-rs-core --release --features heavy --test heavy_decode
#
# Cargo runs each test binary through SDE (its target runner), and SDE follows child processes,
# so the oracle's Python process and core_harness see the same CPU. The CPUs, and what the
# wheels choose on them:
#   nhm  Nehalem          SSE4.2               DWA's SSE2 paths; OpenJPH's SSE4.1 level; zstd's
#                                              portable Huffman loops (no BMI2)
#   snb  Sandy Bridge     AVX, no F16C         Linux: DWA's AVX inverse DCT, scalar halves;
#                                              OpenJPH's AVX level
#   ivb  Ivy Bridge       AVX and F16C         Linux: DWA's and unpacking's F16C conversions
#   hsw  Haswell          AVX2, BMI1/2, FMA    OpenJPH's AVX2 kernels; Linux: zstd's assembly
#                                              Huffman loop
#   skx  Skylake server   AVX-512              OpenJPH's AVX-512 kernels
# (Windows: Core's CPU probe never reports AVX or F16C, so DWA and unpacking take their SSE2
# and scalar paths on every CPU; OpenJPH's probe works there and still chooses by CPU.)
# `sde -help` lists the other CPUs. Set the oracle up natively first (`uv sync --project
# oracle`) and build what the run needs: under SDE everything runs many times slower.
#
# SDE comes from Intel's download mirror under Intel's license, which the owner accepted (as for
# OpenColorIO-rs). The archive is downloaded on first use into target/sde-archives/ (shared by
# Windows and the Rocky container) and checked against the SHA256 pinned here before it is
# unpacked into <cargo target dir>/sde/.
set -euo pipefail

if [ $# -lt 2 ]; then
    echo "usage: scripts/sde.sh <cpu> <command...>   (cpu: nhm, snb, ivb, hsw, skx, ...)" >&2
    exit 2
fi
cpu="$1"
shift

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="10.13.1-2026-07-28"
mirror="https://downloadmirror.intel.com/924984"
case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*)
        os=win
        sha256=74e626ede09b0baa5011fc9e51b58627ea92c3fc0bae5fd7db34b490f335f651
        exe=sde.exe
        runner_var=CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUNNER
        ;;
    Linux)
        os=lin
        sha256=94e97d623fec54385686e1e7ba65ebc9941748c05ee451423948334892bf2b50
        exe=sde64
        runner_var=CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER
        ;;
    *)
        echo "scripts/sde.sh: unsupported OS $(uname -s)" >&2
        exit 2
        ;;
esac

archive="sde-external-$version-$os.tar.xz"
archives="$root/target/sde-archives"
kits="${CARGO_TARGET_DIR:-$root/target}/sde"
kit="$kits/sde-external-$version-$os"

if [ ! -f "$kit/$exe" ]; then
    mkdir -p "$archives" "$kits"
    if [ ! -f "$archives/$archive" ]; then
        echo "scripts/sde.sh: downloading $mirror/$archive" >&2
        curl -fsSL --retry 3 -o "$archives/$archive.part" "$mirror/$archive"
        mv "$archives/$archive.part" "$archives/$archive"
    fi
    if ! echo "$sha256  $archives/$archive" | sha256sum --check --quiet -; then
        echo "scripts/sde.sh: $archive does not have the pinned SHA256 $sha256" >&2
        exit 1
    fi
    # GNU tar reads "D:/..." as a remote host; --force-local keeps Windows paths local.
    tar --force-local -xJf "$archives/$archive" -C "$kits"
fi

sde="$kit/$exe"
# Each emulated oracle process takes about 1 GB of memory; with one test thread per core,
# OpenColorIO-rs's runs ran out ("Pin is out of memory").
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"
# The heavy tests take their smaller image set (the wheel's write_images layouts, every
# compression; no corpus or damaged copies) under SDE.
export OPENEXR_RS_SDE="$cpu"
flags=("-$cpu")
if [ "$os" = win ]; then
    sde="$(cygpath -m "$sde")"
    # The MSVC runtime that ships with Python picks its memcpy from the CPU features Windows
    # reports, not from CPUID, so it runs AVX code on every emulated CPU and SDE's chip check
    # stops it. CPUID, which SDE emulates, still decides every path of Core, its libraries and
    # the port.
    flags+=(-chip_check_disable 1)
fi
export "$runner_var=$sde ${flags[*]} --"
exec "$@"
