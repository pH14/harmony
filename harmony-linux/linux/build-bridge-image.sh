#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **`/dev/harmony` bridge-liveness initramfs** (hm-i8kc F2): a static
# busybox, the shipped `libvoidstar.so`, the dynamically-linked `bridge-probe`
# (it dlopens that library, so unlike the flow image a loader closure DOES ride
# in — the play-agent pattern), and `bridge-init.sh` as /init. Produces
# harmony-linux/build/initramfs-bridge.cpio.gz.
#
# The companion kernel must be built from THIS tree: `/dev/harmony` comes from
# the char-device patch (patches/x86/0002-x86-harmony-character-device.patch,
# CONFIG_HARMONY_DEVICE=y in config-fragment), both landed 2026-07-20 in PR #133.
# Every bzImage built before that date — including the content-pinned PR-44
# kernel the task-68/95 gates boot — has no such device, so:
#
#   make -C harmony-linux/linux kernel        # bzImage WITH CONFIG_HARMONY_DEVICE=y
#   harmony-linux/linux/build-bridge-image.sh
#
# Run (box-only — needs patched KVM + a leased core):
#   taskset -c <leased-core> campaign-runner box --kernel bzImage \
#       --initramfs initramfs-bridge.cpio.gz --ready-marker BRIDGE_DONE \
#       --seeds 4 --runs 2 --deadline-delta 20000000
# then read the `BRIDGE_*` serial lines. Note the F10 ordering: the probe fires
# during `drive_to_marker`, before `ControlServer::new` wires `enable_sdk`, so on
# an unfixed host the Event/Entropy services answer `UnknownService` and the
# probe fails by design — that failure IS the F10 evidence.
#
# Linux/x86_64 only; does NOT need root (no mke2fs here).
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip cpio ldd

BRIDGEROOT=$BUILD_ROOT/bridge-root

# --- 1. static busybox (mirrors build-flow-image.sh) --------------------------
echo "== bridge image: building static busybox ($BUSYBOX_VERSION)"
extract_busybox
mkdir -p "$BBOBJ" "$ART_DIR"
make -C "$BBSRC" O="$BBOBJ" defconfig >/dev/null
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$BBOBJ/.config" >"$BBOBJ/.config.tmp"
mv "$BBOBJ/.config.tmp" "$BBOBJ/.config"
set +o pipefail            # yes(1) dies of SIGPIPE — judge by make's status alone
yes '' | make -C "$BBSRC" O="$BBOBJ" oldconfig >/dev/null
set -o pipefail
grep -qxF 'CONFIG_STATIC=y' "$BBOBJ/.config" || { echo "FAIL: busybox not static" >&2; exit 1; }
make -C "$BBSRC" O="$BBOBJ" -j"$(nproc)" busybox >/dev/null

# --- 2. assemble the guest rootfs ---------------------------------------------
echo "== bridge image: assembling rootfs"
rm -rf "$BRIDGEROOT"
mkdir -p "$BRIDGEROOT"/{bin,etc,proc,sys,dev,tmp,lib64,opt/harmony}
mkdir -p "$BRIDGEROOT/lib/x86_64-linux-gnu" "$BRIDGEROOT/usr/lib"

cp "$BBOBJ/busybox" "$BRIDGEROOT/bin/busybox"
for a in sh mount umount mkdir chmod cat echo ls grep head tee printf od \
         reboot halt true false test sync; do
    ln -sf busybox "$BRIDGEROOT/bin/$a"
done

# libvoidstar at its fixed ABI path (the probe dlopens /usr/lib/libvoidstar.so).
install_libvoidstar "$BRIDGEROOT"

# --- 3. the probe + its dynamic-loader closure --------------------------------
# Dynamic on purpose: dlopen needs a loader, and the point of the libvoidstar leg
# is to exercise the SHIPPED shared object the way a real SDK guest loads it.
echo "== bridge image: building bridge-probe"
PROBE=$BUILD_ROOT/bridge-probe
cc -O2 -Wall -Wextra -Werror -o "$PROBE" "$LINUX_DIR/bridge-probe.c"
install -m 0755 "$PROBE" "$BRIDGEROOT/opt/harmony/bridge-probe"

cp -L /lib64/ld-linux-x86-64.so.2 "$BRIDGEROOT/lib64/"
ldd "$PROBE" | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/bridge-libs.txt"
while read -r so; do
    [ -e "$so" ] && cp -L "$so" "$BRIDGEROOT/lib/x86_64-linux-gnu/$(basename "$so")"
done <"$BUILD_ROOT/bridge-libs.txt"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$BRIDGEROOT/etc/passwd"
printf 'root:x:0:\n' >"$BRIDGEROOT/etc/group"

install -m 0755 "$LINUX_DIR/bridge-init.sh" "$BRIDGEROOT/init"

# --- 4. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
echo "== bridge image: packing initramfs"
find "$BRIDGEROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$BRIDGEROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-bridge.cpio.gz"
echo "ok: $ART_DIR/initramfs-bridge.cpio.gz ($(du -h "$ART_DIR/initramfs-bridge.cpio.gz" | cut -f1))"
