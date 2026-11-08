#!/usr/bin/env bash
# SPDX-License-Identifier: BSD-3-Clause
# Copyright Contributors to the OpenEXR Project.
#
# Fetches the corpus (PLAN.md §8): AcademySoftwareFoundation/openexr-images at the pinned commit,
# into corpus/openexr-images (not committed). Run again, it fetches nothing new and checks the
# checkout is still the pinned commit.
#
# usage: scripts/fetch-corpus.sh
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dir="$root/corpus/openexr-images"
url=https://github.com/AcademySoftwareFoundation/openexr-images.git
commit=e38ffb0790f62f05a6f083a6fa4cac150b3b7452

if [ ! -d "$dir/.git" ]; then
    mkdir -p "$dir"
    git -C "$dir" init -q
    git -C "$dir" remote add origin "$url"
fi
if [ "$(git -C "$dir" rev-parse -q --verify HEAD 2>/dev/null || true)" != "$commit" ]; then
    git -C "$dir" fetch -q --depth 1 origin "$commit"
    git -C "$dir" checkout -q --detach FETCH_HEAD
fi
head="$(git -C "$dir" rev-parse HEAD)"
if [ "$head" != "$commit" ]; then
    echo "fetch-corpus: $dir is at $head, not the pinned $commit" >&2
    exit 1
fi
if [ -n "$(git -C "$dir" status --porcelain)" ]; then
    echo "fetch-corpus: $dir has local changes" >&2
    exit 1
fi
echo "corpus/openexr-images at $commit: $(find "$dir" -name '*.exr' | wc -l) images"
