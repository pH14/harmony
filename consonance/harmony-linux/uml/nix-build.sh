#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build User-mode Linux artifacts from the locked Nix closure into an empty
# output directory. The flake supplies the toolchain and the pinned sources.
set -euo pipefail

[ "$#" -eq 2 ] && [ "$1" = --output ] || {
    echo "usage: harmony-build-uml --output DIR" >&2
    exit 2
}
output=$2
if [ -e "$output" ] && [ -n "$(find "$output" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]; then
    echo "FAIL: output directory is not empty: $output" >&2
    exit 1
fi
mkdir -p "$output"
output=$(cd "$output" && pwd)

work=$(mktemp -d "${TMPDIR:-/tmp}/harmony-nix-uml.XXXXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
mkdir -p "$work/repo" "$work/downloads" "$work/artifacts" "$work/build"
cp -a "$HARMONY_NIX_SOURCE/." "$work/repo/"
chmod -R u+w "$work/repo"
# shellcheck source=../linux/versions.lock disable=SC1091
. "$work/repo/consonance/harmony-linux/linux/versions.lock"
case "$(uname -m)" in
    aarch64) source_url=$UML_ARM64_URL ;;
    *) source_url=$KERNEL_URL ;;
esac
install -m 0644 "$HARMONY_NIX_LINUX_SOURCE" "$work/downloads/$(basename "$source_url")"
install -m 0644 "$HARMONY_NIX_MUSL_SOURCE" "$work/downloads/$(basename "$MUSL_URL")"

export SOURCE_DATE_EPOCH=0
export TZ=UTC
export LC_ALL=C
export HARMONY_DOWNLOAD_DIR=$work/downloads
export HARMONY_ARTIFACT_DIR=$work/artifacts
export GUEST_BUILD_ROOT=$work/build
"$work/repo/consonance/harmony-linux/uml/build-uml.sh"

arch=$(uname -m)
for name in linux config initramfs.cpio.gz profile.json; do
    cp -p "$work/artifacts/uml/$arch/$name" "$output/$name"
done
echo "ok: $output"
