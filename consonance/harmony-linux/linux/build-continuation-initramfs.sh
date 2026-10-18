#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck source=lib-build.sh
. ./lib-build.sh
require_linux_amd64
require_tools cc gzip
extract_kernel
mkdir -p "$ART_DIR"
cc -O2 -nostdlib -static -fno-stack-protector -fno-pie -no-pie \
    -mgeneral-regs-only -Wl,--build-id=none -o "$BUILD_ROOT/continuation-init" continuation-init.c
cc -O2 -o "$BUILD_ROOT/gen_init_cpio-continuation" "$KSRC/usr/gen_init_cpio.c"
cat >"$BUILD_ROOT/continuation.spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
file /init $BUILD_ROOT/continuation-init 0755 0 0
EOF
"$BUILD_ROOT/gen_init_cpio-continuation" -t 0 "$BUILD_ROOT/continuation.spec" |
    gzip -n -9 >"$ART_DIR/initramfs-continuation.cpio.gz"
