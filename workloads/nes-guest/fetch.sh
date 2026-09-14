#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fetch the open-source emulator input used by the legacy NES package. ROM
# bytes remain caller-provided and are never downloaded by this script.
set -euo pipefail

package_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$package_dir/../.." && pwd)
DL_DIR=${HARMONY_DOWNLOAD_DIR:-$repo_root/consonance/harmony-linux/dl}

# shellcheck source=../../consonance/harmony-linux/scripts/lib.sh disable=SC1091
. "$repo_root/consonance/harmony-linux/scripts/lib.sh"
# shellcheck source=versions.lock disable=SC1091
. "$package_dir/versions.lock"

mkdir -p "$DL_DIR"
file="$DL_DIR/$(basename "$FCEUMM_URL")"
if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$FCEUMM_SHA256" ]; then
    echo "ok: $file (cached, hash verified)"
    exit 0
fi
if command -v curl >/dev/null 2>&1; then
    curl -fsSL --connect-timeout 20 --retry 2 --retry-all-errors --retry-delay 5 \
        -o "$file.part" "$FCEUMM_URL"
elif command -v wget >/dev/null 2>&1; then
    wget -q --connect-timeout=20 --tries=3 --waitretry=5 \
        -O "$file.part" "$FCEUMM_URL"
else
    echo "FAIL: need curl or wget to fetch $FCEUMM_URL" >&2
    exit 1
fi
got=$(sha256_of "$file.part")
if [ "$got" != "$FCEUMM_SHA256" ]; then
    echo "FAIL: $file sha256 mismatch (want $FCEUMM_SHA256, got $got)" >&2
    rm -f "$file.part"
    exit 1
fi
mv "$file.part" "$file"
echo "ok: $file (downloaded, hash verified)"
