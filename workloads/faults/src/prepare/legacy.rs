// SPDX-License-Identifier: AGPL-3.0-or-later
//! Byte-exact image layout used before the diagnostic shell was introduced.
//! Retained workspaces select this layout only by their pinned image digest.

pub(super) const INIT: &[u8] = br##"#!/bin/sh
set -eu
# Kernel init environments need an explicit search path for guest tools.
export PATH=/sbin:/usr/sbin:/bin:/usr/bin
BB=/bin/busybox
stage=bootstrap
# Keep failures before the SDK transport opens visible on the guest console.
# The host can then distinguish an init/mount/chroot failure from a guest that
# reached the agent but stopped before publishing setup_complete.
trap 'rc=$?; echo "FAULT_INIT_EXIT stage=$stage rc=$rc" >&2' 0
echo "FAULT_INIT_STAGE=$stage" >&2
$BB mkdir -p /proc /sys /dev /run /tmp
# Kata's base initramfs has DEVTMPFS_MOUNT enabled, so /dev may already be
# mounted before this package-owned init runs.  Check each target first so a
# pre-mounted filesystem is accepted while a real mount failure still aborts
# startup under `set -e`.
stage=mount-proc
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /proc proc' /proc/mounts 2>/dev/null; then
    $BB mount -t proc proc /proc
fi
stage=mount-sys
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /sys sysfs' /proc/mounts 2>/dev/null; then
    $BB mount -t sysfs sysfs /sys
fi
stage=mount-dev
echo "FAULT_INIT_STAGE=$stage" >&2
if ! $BB grep -q ' /dev devtmpfs' /proc/mounts 2>/dev/null; then
    $BB mount -t devtmpfs dev /dev
fi
ROOT=/harmony-oci/rootfs
stage=prepare-rootfs
echo "FAULT_INIT_STAGE=$stage" >&2
$BB mkdir -p "$ROOT/run" "$ROOT/tmp" "$ROOT/run/fault-agent"
$BB chmod 1777 "$ROOT/tmp"
stage=bind-rootfs
echo "FAULT_INIT_STAGE=$stage" >&2
# iproute2's `ip netns exec` creates a mount namespace and makes `/` a
# recursive slave.  A plain chroot root is not a mountpoint, so that operation
# otherwise fails with EINVAL before the workload can configure its namespaces.
$BB mount --bind "$ROOT" "$ROOT"
stage=bind-pseudo-filesystems
echo "FAULT_INIT_STAGE=$stage" >&2
for directory in dev proc sys; do
    $BB mkdir -p "$ROOT/$directory"
    $BB mount --bind "/$directory" "$ROOT/$directory"
done
stage=chroot-agent
echo "FAULT_INIT_STAGE=$stage" >&2
exec $BB chroot "$ROOT" /opt/harmony/fault-agent \
    --bundle /etc/harmony/bundle --hook-dir /run/fault-agent
"##;
