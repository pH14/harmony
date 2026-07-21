#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Remove build trees and artifacts, keeping downloads (harmony-linux/dl/) and the
# extracted pristine sources — "make clean-artifacts && make image" must work
# without re-downloading.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

rm -rf "$KOBJ" "$BBOBJ" "$BUILD_ROOT/gen_init_cpio" "$BUILD_ROOT/initramfs.spec" \
    "$BUILD_ROOT/libvoidstar-build"
rm -f "$ART_DIR/bzImage" "$ART_DIR/initramfs.cpio.gz"
echo "ok: build trees and artifacts removed (downloads and sources kept)"
