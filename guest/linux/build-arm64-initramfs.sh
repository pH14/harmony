#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the AA-5(c) arm64 initramfs natively on the pinned Altra box: static
# BusyBox + the fixed-marker /init, packed reproducibly with gen_init_cpio.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make gzip bzip2
extract_busybox
extract_kernel # for usr/gen_init_cpio.c

mkdir -p "$ARM64_BBOBJ" "$ARM64_ART_DIR"

echo "== arm64 initramfs: building static busybox ($BUSYBOX_VERSION)"
make -C "$BBSRC" O="$ARM64_BBOBJ" defconfig
# BusyBox keeps the first assignment for a symbol. Rewrite defconfig in place
# rather than appending a conflicting value. tc's obsolete CBQ code does not
# build against the pinned kernel headers, so retain the established exclusion.
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$ARM64_BBOBJ/.config" >"$ARM64_BBOBJ/.config.tmp"
mv "$ARM64_BBOBJ/.config.tmp" "$ARM64_BBOBJ/.config"
set +o pipefail
yes '' | make -C "$BBSRC" O="$ARM64_BBOBJ" oldconfig >/dev/null
set -o pipefail
if ! grep -qxF 'CONFIG_STATIC=y' "$ARM64_BBOBJ/.config"; then
    echo "FAIL: CONFIG_STATIC=y did not stick in the arm64 busybox config" >&2
    exit 1
fi
if grep -qxF 'CONFIG_TC=y' "$ARM64_BBOBJ/.config"; then
    echo "FAIL: CONFIG_TC=y did not turn off in the arm64 busybox config" >&2
    exit 1
fi
make -C "$BBSRC" O="$ARM64_BBOBJ" -j"$(nproc)" busybox

echo "== arm64 initramfs: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio-arm64" "$KSRC/usr/gen_init_cpio.c"

spec=$BUILD_ROOT/initramfs-arm64.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /bin 0755 0 0
file /bin/busybox $ARM64_BBOBJ/busybox 0755 0 0
slink /bin/sh /bin/busybox 0777 0 0
file /init $LINUX_DIR/arm64-init.sh 0755 0 0
EOF

"$BUILD_ROOT/gen_init_cpio-arm64" -t 0 "$spec" \
    | gzip -n -9 >"$ARM64_ART_DIR/initramfs.cpio.gz"
echo "ok: $ARM64_ART_DIR/initramfs.cpio.gz"
