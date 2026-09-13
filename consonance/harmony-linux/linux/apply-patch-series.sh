#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Apply an ordered patch series once to an extracted kernel source tree.
set -euo pipefail
source_tree=${1:?source tree required}
shift
[ "$#" -gt 0 ] || { echo "FAIL: at least one patch directory is required" >&2; exit 1; }

patch_dirs=()
patches=()
for patch_dir_arg in "$@"; do
    patch_dir=$(cd "$patch_dir_arg" && pwd) || {
        echo "FAIL: patch directory does not exist: $patch_dir_arg" >&2
        exit 1
    }
    patch_dirs+=("$patch_dir")
    patches_before=${#patches[@]}
    while IFS= read -r -d '' patch_file; do
        patches+=("$patch_file")
    done < <(find "$patch_dir" -maxdepth 1 -type f \
        -name '[0-9][0-9][0-9][0-9]-*.patch' -print0 | LC_ALL=C sort -z)
    if [ "${#patches[@]}" -eq "$patches_before" ]; then
        echo "FAIL: no numbered patches in $patch_dir" >&2
        exit 1
    fi
done

if [ "${#patches[@]}" -eq 0 ]; then
    echo "FAIL: patch series is empty" >&2
    exit 1
fi
stamp=$source_tree/.harmony-patch-series
pending=$stamp.pending
# Include names and content, but not checkout-specific paths.
series=$(for patch_file in "${patches[@]}"; do
    printf '%s/%s\n' "$(basename "$(dirname "$patch_file")")" "${patch_file##*/}"
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
