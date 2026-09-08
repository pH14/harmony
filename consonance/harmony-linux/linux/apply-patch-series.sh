#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Apply an ordered patch series once to an extracted kernel source tree.
set -euo pipefail
source_tree=${1:?source tree required}
patch_dir=$(cd "${2:?patch directory required}" && pwd)
patches=("$patch_dir"/[0-9][0-9][0-9][0-9]-*.patch)
[ -f "${patches[0]}" ] || { echo "FAIL: no numbered patches in $patch_dir" >&2; exit 1; }
stamp=$source_tree/.harmony-patch-series
pending=$stamp.pending
# Include names and content, but not checkout-specific paths.
series=$(for patch_file in "${patches[@]}"; do
    printf '%s\n' "${patch_file##*/}"
    shasum -a 256 < "$patch_file"
done)
if [ -e "$pending" ]; then
    echo "FAIL: incomplete patch series; delete $source_tree and re-extract" >&2
    exit 1
fi
if [ -f "$stamp" ]; then
    if [ "$(cat "$stamp")" != "$series" ]; then
        echo "FAIL: patch series changed; delete $source_tree and re-extract" >&2
        exit 1
    fi
    echo "== kernel: matching patch series already applied"
    exit 0
fi
# Leave the pending marker on failure: a partially patched tree is never adopted.
printf '%s\n' "$series" > "$pending"
for patch_file in "${patches[@]}"; do
    echo "== kernel: applying ${patch_file##*/}"
    (cd "$source_tree" && patch -p1 --dry-run --force < "$patch_file")
    (cd "$source_tree" && patch -p1 --force < "$patch_file")
done
mv "$pending" "$stamp"
