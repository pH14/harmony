#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **SMB game workload initramfs** (task 86): a static busybox, the
# commit-pinned QuickNES libretro core (built here from the verified tarball —
# never vendored), the play-agent (a dynamic glibc binary: it dlopens the core,
# which a fully-static musl build cannot do — its ldd closure is copied in, the
# build-postgres-image.sh pattern), the user-supplied ROM, and game-init.sh as
# /init. Produces guest/build/initramfs-game.cpio.gz.
#
# ROM discipline (task 86, hard requirement): the SMB ROM is copyrighted and is
# never committed, vendored, or fetched. It enters ONLY via HARMONY_SMB_ROM=
# <path> (a user-supplied dump); when unset the image builds WITHOUT the game
# workload and prints a loud SKIP — game-init.sh then reports GAME_SKIP at boot.
# The ROM's sha256 is baked into the image (and echoed here) so campaign
# reports are comparable across runs of the same dump.
#
# The companion kernel is the unchanged container-class bzImage (`make kernel`):
# hugetlbfs, /dev/mem, iopl and pagemap are all already available.
#
# Linux/x86_64 only (builds the core + the agent natively — the guest and the
# box are the same platform); does NOT need root (no mke2fs here).
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc c++ make gzip cpio ldd cargo

GAMEROOT=$BUILD_ROOT/game-root
CORESRC=$BUILD_ROOT/QuickNES_Core-$QUICKNES_COMMIT
CORE_SO=$CORESRC/quicknes_libretro.so

# --- 1. static busybox (mirrors build-campaign-image.sh) ---------------------
echo "== game image: building static busybox ($BUSYBOX_VERSION)"
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

# --- 2. the commit-pinned QuickNES core (verify -> extract -> make) ----------
echo "== game image: building QuickNES core (pin $QUICKNES_COMMIT)"
verify_and_extract "$DL_DIR/$(basename "$QUICKNES_URL")" "$QUICKNES_SHA256" "$CORESRC"
make -C "$CORESRC" platform=unix -j"$(nproc)" >/dev/null
[ -f "$CORE_SO" ] || { echo "FAIL: QuickNES core did not build ($CORE_SO missing)" >&2; exit 1; }

# --- 3. the play-agent (dynamic glibc; dlopens the core at runtime) ----------
# PLAY_AGENT_BIN= a prebuilt binary skips the in-tree build (the k3s-image
# FLOW_AGENT_BIN pattern); by default build it from guest/play-agent/.
if [ -n "${PLAY_AGENT_BIN:-}" ]; then
    echo "== game image: using prebuilt play-agent: $PLAY_AGENT_BIN"
    AGENT_BIN=$PLAY_AGENT_BIN
else
    echo "== game image: building play-agent (guest/play-agent)"
    AGENT_BIN=$(bash "$GUEST_DIR/play-agent/build.sh" | tail -1)
fi
[ -x "$AGENT_BIN" ] || { echo "FAIL: play-agent binary missing ($AGENT_BIN)" >&2; exit 1; }

# --- 4. assemble the guest rootfs --------------------------------------------
echo "== game image: assembling rootfs"
rm -rf "$GAMEROOT"
mkdir -p "$GAMEROOT"/{bin,lib,lib64,etc,proc,sys,dev,tmp,opt/harmony}
mkdir -p "$GAMEROOT/lib/x86_64-linux-gnu"

cp "$BBOBJ/busybox" "$GAMEROOT/bin/busybox"
for a in sh mount umount mkdir chmod cat echo ls grep head tee printf \
         reboot halt true false test sync; do
    ln -sf busybox "$GAMEROOT/bin/$a"
done

install -m 0755 "$AGENT_BIN" "$GAMEROOT/opt/harmony/play-agent"
install -m 0644 "$CORE_SO" "$GAMEROOT/opt/harmony/quicknes_libretro.so"

# The dynamic loader + the shared-lib closure of the agent AND the core (the
# core is dlopen'd, so ldd the .so too — its libstdc++/libgcc_s/libm ride in).
cp -L /lib64/ld-linux-x86-64.so.2 "$GAMEROOT/lib64/"
{ ldd "$AGENT_BIN"; ldd "$CORE_SO"; } 2>/dev/null \
    | awk '/=> \// {print $3}' | sort -u >"$BUILD_ROOT/game-libs.txt"
while read -r so; do
    [ -e "$so" ] && cp -L "$so" "$GAMEROOT/lib/x86_64-linux-gnu/$(basename "$so")"
done <"$BUILD_ROOT/game-libs.txt"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$GAMEROOT/etc/passwd"
printf 'root:x:0:\n' >"$GAMEROOT/etc/group"

# --- 5. the user-supplied ROM (HARMONY_SMB_ROM) — or a loud SKIP -------------
if [ -n "${HARMONY_SMB_ROM:-}" ]; then
    [ -f "$HARMONY_SMB_ROM" ] || { echo "FAIL: HARMONY_SMB_ROM=$HARMONY_SMB_ROM does not exist" >&2; exit 1; }
    install -m 0644 "$HARMONY_SMB_ROM" "$GAMEROOT/opt/harmony/smb.nes"
    ROM_SHA=$(sha256_of "$GAMEROOT/opt/harmony/smb.nes")
    printf '%s\n' "$ROM_SHA" >"$GAMEROOT/opt/harmony/smb.nes.sha256"
    echo "== game image: ROM baked in (sha256 $ROM_SHA — record this in the campaign report)"
else
    echo "== game image: SKIP — HARMONY_SMB_ROM unset; building WITHOUT the game ROM." >&2
    echo "   The image boots and reports GAME_SKIP; every task-86 box gate reports" >&2
    echo "   SKIP until a user-supplied ROM is provided (a skipped gate is not green)." >&2
fi

install -m 0755 "$LINUX_DIR/game-init.sh" "$GAMEROOT/init"

# --- 6. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
echo "== game image: packing initramfs"
find "$GAMEROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$GAMEROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-game.cpio.gz"
echo "ok: $ART_DIR/initramfs-game.cpio.gz ($(du -h "$ART_DIR/initramfs-game.cpio.gz" | cut -f1))"

# --- 7. the film-renderer core copy (task 87 shares the pin) ------------------
# The host-side film renderer dlopens the SAME built core (HARMONY_SMB_CORE=);
# export it beside the initramfs so the box gate uses one artifact for both.
install -m 0644 "$CORE_SO" "$ART_DIR/quicknes_libretro.so"
echo "ok: $ART_DIR/quicknes_libretro.so (the shared task-86/87 core pin)"
