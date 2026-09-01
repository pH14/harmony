#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **flow-agent doorbell-firing initramfs** (hm-rdp): a static busybox,
# the static-musl flow agent (the maze/k3s build pattern — nothing here dlopens,
# so no dynamic loader rides in), and flow-init.sh as /init. Produces
# consonance/harmony-linux/build/initramfs-flow.cpio.gz.
#
# The companion kernel is the unchanged container-class bzImage (`make kernel` —
# the same image the maze/game workloads boot): /dev/mem and iopl (the doorbell
# transport's needs, CONFIG_DEVMEM + CONFIG_X86_IOPL_IOPERM) are already built in.
# This is the minimal vehicle for the first-ever live validation that the
# flow-agent doorbell path executes on that kernel — see flow-init.sh and hm-rdp.
#
# Run (validates the firing; box-only — needs patched KVM + a leased core):
#   consonance/harmony-linux/linux/build-flow-image.sh
#   taskset -c <leased-core> campaign-runner box --kernel bzImage \
#       --initramfs initramfs-flow.cpio.gz --ready-marker FLOW_DONE \
#       --seeds 8 --runs 2 --deadline-delta 20000000
# then read the `flow-agent:` serial lines (a `Net doorbell unwired -> nominal`
# with no iopl/`/dev/mem` error still proves the guest-side path executed).
#
# Linux/x86_64 only (builds the agent natively — the guest and the box are the
# same platform); does NOT need root (no mke2fs here).
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip cpio cargo

FLOWROOT=$BUILD_ROOT/flow-root

# --- 1. static busybox (mirrors build-maze-image.sh) --------------------------
echo "== flow image: building static busybox ($BUSYBOX_VERSION)"
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

# --- 2. the flow agent (static musl; the maze-agent pattern) ------------------
# FLOW_AGENT_BIN= a prebuilt binary skips the in-tree build (the k3s-image
# pattern); by default build it from consonance/harmony-linux/flow-agent/.
if [ -n "${FLOW_AGENT_BIN:-}" ]; then
    echo "== flow image: using prebuilt flow-agent: $FLOW_AGENT_BIN"
    AGENT_BIN=$FLOW_AGENT_BIN
else
    echo "== flow image: building flow-agent (consonance/harmony-linux/flow-agent)"
    AGENT_BIN=$(sh "$GUEST_DIR/flow-agent/build-static.sh" | tail -1)
fi
[ -x "$AGENT_BIN" ] || { echo "FAIL: flow-agent binary missing ($AGENT_BIN)" >&2; exit 1; }

# --- 3. assemble the guest rootfs ---------------------------------------------
echo "== flow image: assembling rootfs"
rm -rf "$FLOWROOT"
mkdir -p "$FLOWROOT"/{bin,etc,proc,sys,dev,tmp,opt/harmony}

cp "$BBOBJ/busybox" "$FLOWROOT/bin/busybox"
for a in sh mount umount mkdir chmod cat echo ls grep head tee printf \
         reboot halt true false test sync; do
    ln -sf busybox "$FLOWROOT/bin/$a"
done

install -m 0755 "$AGENT_BIN" "$FLOWROOT/opt/harmony/flow-agent"

printf 'root:x:0:0:root:/root:/bin/sh\n' >"$FLOWROOT/etc/passwd"
printf 'root:x:0:\n' >"$FLOWROOT/etc/group"

install -m 0755 "$LINUX_DIR/flow-init.sh" "$FLOWROOT/init"

# --- 4. pack the initramfs (sorted, fixed mtime, owner 0:0, gzip -n) ----------
echo "== flow image: packing initramfs"
find "$FLOWROOT" -mindepth 1 -exec touch -hcd @0 {} +
( cd "$FLOWROOT" && find . -mindepth 1 -print0 | LC_ALL=C sort -z \
    | cpio --null -o -H newc --owner=0:0 --quiet ) | gzip -n -9 >"$ART_DIR/initramfs-flow.cpio.gz"
echo "ok: $ART_DIR/initramfs-flow.cpio.gz ($(du -h "$ART_DIR/initramfs-flow.cpio.gz" | cut -f1))"
