#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the experimental x86 Nova-in-Consonance initramfs. The caller supplies
# the pinned FOSS ROM and QuickNES artifacts built by dissonance's verified
# scripts; this builder adds the guest play-agent and its dynamic-library
# closure, then packs a reproducible initramfs.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip cpio ldd cargo

: "${HARMONY_NOVA_ROM:?set HARMONY_NOVA_ROM to the pinned built nova.nes}"
: "${HARMONY_NOVA_CORE:?set HARMONY_NOVA_CORE to the pinned QuickNES libretro core}"
[ -f "$HARMONY_NOVA_ROM" ] || { echo "FAIL: Nova ROM missing: $HARMONY_NOVA_ROM" >&2; exit 1; }
[ -f "$HARMONY_NOVA_CORE" ] || { echo "FAIL: QuickNES core missing: $HARMONY_NOVA_CORE" >&2; exit 1; }

NOVA_ROOT=$BUILD_ROOT/nova-game-root

echo "== Nova game image: building static busybox ($BUSYBOX_VERSION)"
extract_busybox
mkdir -p "$BBOBJ" "$ART_DIR"
make -C "$BBSRC" O="$BBOBJ" defconfig >/dev/null
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$BBOBJ/.config" >"$BBOBJ/.config.tmp"
mv "$BBOBJ/.config.tmp" "$BBOBJ/.config"
set +o pipefail
yes '' | make -C "$BBSRC" O="$BBOBJ" oldconfig >/dev/null
set -o pipefail
grep -qxF 'CONFIG_STATIC=y' "$BBOBJ/.config" || { echo "FAIL: busybox not static" >&2; exit 1; }
make -C "$BBSRC" O="$BBOBJ" -j"$(nproc)" busybox >/dev/null

if [ -n "${PLAY_AGENT_BIN:-}" ]; then
    AGENT_BIN=$PLAY_AGENT_BIN
else
    echo "== Nova game image: building guest play-agent"
    AGENT_BIN=$(bash "$GUEST_DIR/play-agent/build.sh" | tail -1)
fi
[ -x "$AGENT_BIN" ] || { echo "FAIL: play-agent binary missing: $AGENT_BIN" >&2; exit 1; }

echo "== Nova game image: assembling rootfs"
rm -rf "$NOVA_ROOT"
mkdir -p "$NOVA_ROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,opt/harmony}
mkdir -p "$NOVA_ROOT/lib/x86_64-linux-gnu"
install_libvoidstar "$NOVA_ROOT"
cp "$BBOBJ/busybox" "$NOVA_ROOT/bin/busybox"
for applet in sh mount umount mkdir chmod cat echo ls grep head tee printf \
    reboot halt true false test sync; do
    ln -sf busybox "$NOVA_ROOT/bin/$applet"
done

install -m 0755 "$AGENT_BIN" "$NOVA_ROOT/opt/harmony/play-agent"
install -m 0644 "$HARMONY_NOVA_CORE" "$NOVA_ROOT/opt/harmony/quicknes_libretro.so"
install -m 0644 "$HARMONY_NOVA_ROM" "$NOVA_ROOT/opt/harmony/nova.nes"

cp -L /lib64/ld-linux-x86-64.so.2 "$NOVA_ROOT/lib64/"
{ ldd "$AGENT_BIN"; ldd "$HARMONY_NOVA_CORE"; } 2>/dev/null \
    | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/nova-game-libs.txt"
while read -r shared_object; do
    [ -e "$shared_object" ] && \
        cp -L "$shared_object" "$NOVA_ROOT/lib/x86_64-linux-gnu/$(basename "$shared_object")"
done <"$BUILD_ROOT/nova-game-libs.txt"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$NOVA_ROOT/etc/passwd"
printf 'root:x:0:\n' >"$NOVA_ROOT/etc/group"
ROM_SHA=$(sha256_of "$NOVA_ROOT/opt/harmony/nova.nes")
printf '%s\n' "$ROM_SHA" >"$NOVA_ROOT/opt/harmony/nova.nes.sha256"
install -m 0755 "$LINUX_DIR/nova-game-init.sh" "$NOVA_ROOT/init"

echo "== Nova game image: packing reproducible initramfs (ROM $ROM_SHA)"
find "$NOVA_ROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$NOVA_ROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) \
    | gzip -n -9 >"$ART_DIR/initramfs-nova.cpio.gz"
echo "ok: $ART_DIR/initramfs-nova.cpio.gz ($(du -h "$ART_DIR/initramfs-nova.cpio.gz" | cut -f1))"
