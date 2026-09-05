#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init for the experimental Nova-in-Consonance workload. QuickNES and the
# FOSS Nova ROM run as an ordinary Linux guest process; the guest obtains exact
# controller chords through the Harmony SDK and yields after each chord. The
# host VMM, not QuickNES, owns snapshot and restore.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console

# One physically contiguous 2 MiB billboard page, plus one spare reservation.
echo 2 >/proc/sys/vm/nr_hugepages
hugepages_total=
while read -r key value _unit; do
    case "$key" in
        HugePages_Total:) hugepages_total=$value ;;
    esac
done </proc/meminfo
if [ "$hugepages_total" != 2 ]; then
    echo "NOVA_CONSONANCE_FAIL: unable to reserve two billboard hugepages"
    exec $BB reboot -f
fi

if [ ! -f /opt/harmony/nova.nes ] || [ ! -x /opt/harmony/play-agent ]; then
    echo "NOVA_CONSONANCE_FAIL: guest image lacks Nova ROM or static QuickNES agent"
    exec $BB reboot -f
fi
echo "NOVA_ROM_SHA256: $($BB cat /opt/harmony/nova.nes.sha256)"
echo "NOVA_CONSONANCE_READY: launching QuickNES payload agent"
/opt/harmony/play-agent \
    --nova-payload \
    --core builtin:quicknes \
    --rom /opt/harmony/nova.nes
rc=$?
echo "NOVA_CONSONANCE_EXIT: play-agent exited rc=$rc"

if [ "$rc" != "0" ]; then
    exec $BB reboot -f
fi
exec $BB halt -f
