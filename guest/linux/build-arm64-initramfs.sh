#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the AA-5(c) arm64 initramfs natively on the pinned Altra box: a
# freestanding syscall-only /init, packed reproducibly with gen_init_cpio.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc gzip objdump python3
extract_kernel # for usr/gen_init_cpio.c

mkdir -p "$ARM64_ART_DIR"

echo "== arm64 initramfs: building freestanding LSE-only init"
arm64_init=$BUILD_ROOT/arm64-init
cc -Os -Wall -Wextra -Werror -nostdlib -static -ffreestanding -fno-builtin \
    -fno-stack-protector -fno-pie -march=armv8.1-a+lse -mno-outline-atomics \
    -no-pie -Wl,-e,_start -Wl,--build-id=none -Wl,-z,noexecstack \
    -o "$arm64_init" "$LINUX_DIR/arm64-init.c"
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa4-exclusive-scan.py" "$arm64_init"

echo "== arm64 initramfs: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio-arm64" "$KSRC/usr/gen_init_cpio.c"

spec=$BUILD_ROOT/initramfs-arm64.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /sys 0755 0 0
file /init $arm64_init 0755 0 0
EOF

"$BUILD_ROOT/gen_init_cpio-arm64" -t 0 "$spec" \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs.cpio.gz"
echo "ok: $ARM64_ART_DIR/initramfs.cpio.gz"
