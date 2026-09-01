#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the experimental x86 Nova-in-Consonance initramfs. The caller supplies
# the pinned FOSS ROM and static QuickNES archive built by dissonance's verified
# scripts; this builder links both into one static guest agent, then packs a
# reproducible initramfs.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip cpio readelf cargo

: "${HARMONY_NOVA_ROM:?set HARMONY_NOVA_ROM to the pinned built nova.nes}"
: "${HARMONY_NOVA_CORE_STATIC:?set HARMONY_NOVA_CORE_STATIC to the pinned QuickNES archive}"
[ -f "$HARMONY_NOVA_ROM" ] || { echo "FAIL: Nova ROM missing: $HARMONY_NOVA_ROM" >&2; exit 1; }
[ -f "$HARMONY_NOVA_CORE_STATIC" ] || { echo "FAIL: QuickNES archive missing: $HARMONY_NOVA_CORE_STATIC" >&2; exit 1; }

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
    echo "== Nova game image: building static QuickNES guest play-agent"
    AGENT_BIN=$(HARMONY_QUICKNES_STATIC_LIB="$HARMONY_NOVA_CORE_STATIC" \
        bash "$GUEST_DIR/play-agent/build.sh" | tail -1)
fi
[ -x "$AGENT_BIN" ] || { echo "FAIL: play-agent binary missing: $AGENT_BIN" >&2; exit 1; }
if readelf -l "$AGENT_BIN" | grep -q ' INTERP '; then
    echo "FAIL: Nova play-agent has a dynamic loader" >&2
    exit 1
fi

echo "== Nova game image: assembling rootfs"
rm -rf "$NOVA_ROOT"
mkdir -p "$NOVA_ROOT"/{bin,etc,proc,sys,dev,tmp,opt/harmony}
cp "$BBOBJ/busybox" "$NOVA_ROOT/bin/busybox"
for applet in sh mount umount mkdir chmod cat echo ls grep head tee printf \
    reboot halt true false test sync; do
    ln -sf busybox "$NOVA_ROOT/bin/$applet"
done

install -m 0755 "$AGENT_BIN" "$NOVA_ROOT/opt/harmony/play-agent"
install -m 0644 "$HARMONY_NOVA_ROM" "$NOVA_ROOT/opt/harmony/nova.nes"

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
