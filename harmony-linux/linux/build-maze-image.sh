#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **maze workload initramfs** (task 134): a static busybox, the
# static-musl maze agent (the flow-agent build pattern — nothing here dlopens,
# so no dynamic loader or ldd closure rides in), and maze-init.sh as /init.
# Produces harmony-linux/build/initramfs-maze.cpio.gz.
#
# The companion kernel is the unchanged container-class bzImage (`make
# kernel`): /dev/mem and iopl (the doorbell transport's needs) are already
# available. No ROM, no billboard, no hugetlb — the maze is the simplest
# cooperative workload image in the tree.
#
# Linux/x86_64 only (builds the agent natively — the guest and the box are the
# same platform); does NOT need root (no mke2fs here).
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip cpio cargo

MAZEROOT=$BUILD_ROOT/maze-root

# --- 1. static busybox (mirrors build-game-image.sh) --------------------------
echo "== maze image: building static busybox ($BUSYBOX_VERSION)"
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

# --- 2. the maze agent (static musl; the flow-agent pattern) ------------------
# MAZE_AGENT_BIN= a prebuilt binary skips the in-tree build (the k3s-image
# FLOW_AGENT_BIN pattern); by default build it from harmony-linux/maze-agent/.
if [ -n "${MAZE_AGENT_BIN:-}" ]; then
    echo "== maze image: using prebuilt maze-agent: $MAZE_AGENT_BIN"
    AGENT_BIN=$MAZE_AGENT_BIN
else
    echo "== maze image: building maze-agent (harmony-linux/maze-agent)"
    AGENT_BIN=$(sh "$GUEST_DIR/maze-agent/build-static.sh" | tail -1)
fi
[ -x "$AGENT_BIN" ] || { echo "FAIL: maze-agent binary missing ($AGENT_BIN)" >&2; exit 1; }

# --- 3. assemble the guest rootfs ---------------------------------------------
echo "== maze image: assembling rootfs"
rm -rf "$MAZEROOT"
mkdir -p "$MAZEROOT"/{bin,etc,proc,sys,dev,tmp,opt/harmony}

cp "$BBOBJ/busybox" "$MAZEROOT/bin/busybox"
for a in sh mount umount mkdir chmod cat echo ls grep head tee printf \
         reboot halt true false test sync; do
    ln -sf busybox "$MAZEROOT/bin/$a"
done

install -m 0755 "$AGENT_BIN" "$MAZEROOT/opt/harmony/maze-agent"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$MAZEROOT/etc/passwd"
printf 'root:x:0:\n' >"$MAZEROOT/etc/group"

install -m 0755 "$LINUX_DIR/maze-init.sh" "$MAZEROOT/init"

# --- 4. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
echo "== maze image: packing initramfs"
find "$MAZEROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$MAZEROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-maze.cpio.gz"
echo "ok: $ART_DIR/initramfs-maze.cpio.gz ($(du -h "$ART_DIR/initramfs-maze.cpio.gz" | cut -f1))"
