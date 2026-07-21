#!/bin/sh
# /init of the minimal guest image: mount the kernel filesystems, announce
# readiness on the console, power off. `poweroff -f` calls reboot(2) directly.
# If anything fails, set -e makes init exit, the kernel panics ("attempted to
# kill init"), and panic=-1 + -no-reboot turn that into a QEMU exit — so the
# boot gate fails instead of hanging.
set -e
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
echo GUEST_READY
exec /bin/busybox poweroff -f
