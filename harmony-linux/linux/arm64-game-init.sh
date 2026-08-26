#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# M2 arm64 TetaNES workload init. A ROM-less image is a loud blocked gate, not
# a passing substitute for the live SMB campaign.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB chmod 0666 /dev/console

if [ ! -f /opt/harmony/smb.nes ]; then
    echo "TETANES_GAME_SKIP: HARMONY_SMB_ROM was unset; live M2 cannot run"
    exec $BB halt -f
fi

echo "TETANES_ROM_SHA256: $($BB cat /opt/harmony/smb.nes.sha256)"
echo "TETANES_AGENT_READY: launching TetaNES payload"
/opt/harmony/harmony-tetanes-agent /opt/harmony/smb.nes
rc=$?
echo "TETANES_AGENT_EXIT: rc=$rc"
if [ "$rc" != 0 ]; then
    exec $BB reboot -f
fi
exec $BB halt -f
