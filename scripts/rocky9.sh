#!/usr/bin/env bash
# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# Runs a command in the Rocky Linux 9 reference container, with this checkout at /work.
#   scripts/rocky9.sh scripts/upstream-build.sh Release
#   scripts/rocky9.sh uv run --project oracle python -I oracle/smoke.py
#
# Linux build output lives in a Docker volume per checkout, mounted at /work/target/rocky9 (a fast
# Linux filesystem, never shared with Windows); the uv cache is a shared volume.
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="openexr-rs-rocky9:2"
if ! docker image inspect "$image" >/dev/null 2>&1; then
    docker build -t "$image" "$root/docker/rocky9"
fi

# A directory as the host names it: `pwd -W` prints the Windows path (D:/...) in Git Bash.
host_path() { (cd "$1" && { pwd -W 2>/dev/null || pwd; }); }
canonical() {
    case "$1" in
        [A-Za-z]:/* | //*) printf '%s' "$1" | LC_ALL=C tr 'A-Z' 'a-z' ;;
        *) printf '%s' "$1" ;;
    esac
}

checkout="$(host_path "$root")"
volume="openexr-rs-target-$(canonical "$checkout" | md5sum | cut -c1-12)"

# Git Bash on Windows rewrites paths; keep /work as it is.
export MSYS_NO_PATHCONV=1
if ! docker volume inspect "$volume" >/dev/null 2>&1; then
    docker volume create --label "openexr-rs.checkout=$checkout" "$volume" >/dev/null
fi
tty_flag=""
if [ -t 0 ] && [ -t 1 ]; then tty_flag="-t"; fi
docker run --rm -i $tty_flag \
    -v "$checkout:/work" \
    -v "$volume:/work/target/rocky9" \
    -v openexr-rs-uv-cache:/opt/uv/cache \
    -e UV_PROJECT_ENVIRONMENT=/work/target/rocky9/oracle-venv \
    "$image" "$@"
