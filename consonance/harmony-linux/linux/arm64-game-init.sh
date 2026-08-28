#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# M2 arm64 TetaNES workload init. A ROM-less image is a loud blocked gate, not
# a passing substitute for the live SMB campaign.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
# The sealed HVF board deliberately has only an early PL011 console. Route PID
# 1 and the agent through kmsg, which printk forwards to that early console.
# Seed the nodes so diagnostics survive even if the explicit devtmpfs mount
# fails; a successful mount replaces them with kernel-managed nodes.
$BB mknod -m 0600 /dev/kmsg c 1 11 2>/dev/null
$BB mknod -m 0666 /dev/null c 1 3 2>/dev/null
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB chmod 0600 /dev/kmsg
$BB chmod 0666 /dev/null
exec </dev/null >/dev/kmsg 2>&1

if ! $BB cat /opt/harmony/smb.nes >/dev/null 2>&1; then
    echo "TETANES_GAME_SKIP: HARMONY_SMB_ROM was unset; live M2 cannot run"
    exec $BB halt -f
fi

echo "TETANES_ROM_SHA256: $($BB cat /opt/harmony/smb.nes.sha256)"
echo "TETANES_AGENT_READY: launching TetaNES payload"
/opt/harmony/harmony-tetanes-agent /opt/harmony/smb.nes
rc=$?
echo "TETANES_AGENT_EXIT: rc=$rc"
case "$rc" in
    0) exec $BB halt -f ;;
    *) exec $BB reboot -f ;;
esac
