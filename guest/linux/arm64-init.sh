#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# AA-5(c) boot probe. The harness recognizes only the fixed marker below;
# keeping it in a dedicated init prevents an operator-selected kernel banner
# from satisfying the boot gate. Failure before the marker kills PID 1 and is
# therefore visible as a failed boot rather than a false success.
set -e
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
clocksource=$(/bin/busybox cat /sys/devices/system/clocksource/clocksource0/current_clocksource)
if [ "$clocksource" != harmony-arm-pvclock ]; then
    echo "HARMONY_AA5_FAIL clocksource=$clocksource"
    exec /bin/busybox poweroff -f
fi
echo HARMONY_AA5_CLOCKSOURCE_OK
echo HARMONY_AA5_READY
exec /bin/busybox poweroff -f
