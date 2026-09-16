#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the initramfs: static busybox + /init, packed reproducibly with the
# kernel's own gen_init_cpio (sorted-by-spec entries, owner 0:0, fixed
# mtimes via -t, device nodes without root) and gzip -n.
set -euo pipefail

busybox_binary=
if [ "$#" -gt 0 ]; then
    [ "$#" -eq 2 ] && [ "$1" = --busybox ] || {
        echo "usage: $0 [--busybox FILE]" >&2
        exit 2
    }
    [ -f "$2" ] && [ -r "$2" ] || {
        echo "FAIL: supplied BusyBox is not readable: $2" >&2
        exit 1
    }
    busybox_binary=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
fi

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc gzip readelf
extract_kernel # for usr/gen_init_cpio.c
mkdir -p "$ART_DIR"

if [ -z "$busybox_binary" ]; then
    require_tools make bzip2
    extract_busybox
    prepare_busybox_build_source
    mkdir -p "$BBOBJ"

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
    busybox_binary=$BBOBJ/busybox
fi

busybox_headers=$(readelf -h -l -d "$busybox_binary")
if ! grep -Eq 'Machine:.*Advanced Micro Devices X86-64' <<<"$busybox_headers" ||
    grep -Eq ' INTERP |\(NEEDED\)' <<<"$busybox_headers"; then
    echo "FAIL: minimal initramfs requires a static x86-64 BusyBox" >&2
    exit 1
fi

echo "== initramfs: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio" "$KSRC/usr/gen_init_cpio.c"

spec=$BUILD_ROOT/initramfs.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /bin 0755 0 0
file /bin/busybox $busybox_binary 0755 0 0
slink /bin/sh /bin/busybox 0777 0 0
file /init $LINUX_DIR/init.sh 0755 0 0
EOF

# -t 0: every entry's mtime is SOURCE_DATE_EPOCH-style fixed (0), including
# 'file' entries; gzip -n omits the name/timestamp from the gzip header.
"$BUILD_ROOT/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$ART_DIR/initramfs.cpio.gz"
echo "ok: $ART_DIR/initramfs.cpio.gz"
