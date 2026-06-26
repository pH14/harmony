#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the initramfs: static busybox + /init, packed reproducibly with the
# kernel's own gen_init_cpio (sorted-by-spec entries, owner 0:0, fixed
# mtimes via -t, device nodes without root) and gzip -n.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2
extract_busybox
extract_kernel # for usr/gen_init_cpio.c

mkdir -p "$BBOBJ" "$ART_DIR"

echo "== initramfs: building static busybox ($BUSYBOX_VERSION)"
make -C "$BBSRC" O="$BBOBJ" defconfig
# Tweak defconfig: force a static link, and drop the tc applet (its CBQ code
# does not compile against kernel headers >= 6.8, which removed CBQ).
# busybox's kconfig keeps the *first* value when a symbol is assigned twice,
# so rewrite lines instead of appending.
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$BBOBJ/.config" >"$BBOBJ/.config.tmp"
mv "$BBOBJ/.config.tmp" "$BBOBJ/.config"
# yes(1) dies of SIGPIPE (141) when make closes the pipe — that is expected,
# so judge the pipeline by make's status alone.
set +o pipefail
yes '' | make -C "$BBSRC" O="$BBOBJ" oldconfig >/dev/null
set -o pipefail
if ! grep -qxF 'CONFIG_STATIC=y' "$BBOBJ/.config"; then
    echo "FAIL: CONFIG_STATIC=y did not stick in the busybox config" >&2
    exit 1
fi
if grep -qxF 'CONFIG_TC=y' "$BBOBJ/.config"; then
    echo "FAIL: CONFIG_TC=y did not turn off in the busybox config" >&2
    exit 1
fi
make -C "$BBSRC" O="$BBOBJ" -j"$(nproc)" busybox

echo "== initramfs: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio" "$KSRC/usr/gen_init_cpio.c"

spec=$BUILD_ROOT/initramfs.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /bin 0755 0 0
file /bin/busybox $BBOBJ/busybox 0755 0 0
slink /bin/sh /bin/busybox 0777 0 0
file /init $LINUX_DIR/init.sh 0755 0 0
EOF

# -t 0: every entry's mtime is SOURCE_DATE_EPOCH-style fixed (0), including
# 'file' entries; gzip -n omits the name/timestamp from the gzip header.
"$BUILD_ROOT/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$ART_DIR/initramfs.cpio.gz"
echo "ok: $ART_DIR/initramfs.cpio.gz"
