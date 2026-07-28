#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **`/dev/harmony` bridge-liveness image** (hm-i8kc F2): the minimal
# vehicle for the first real transaction over the guest bridge — a JSON SDK
# emission and a seeded-entropy read, driven both raw (checked return values)
# and through the shipped `/usr/lib/libvoidstar.so` (see bridge-probe.c).
#
# The kernel must carry CONFIG_HARMONY_DEVICE=y (harmony-linux/linux/config-fragment,
# added by PR #133 on 2026-07-20 — every bzImage built before that date has no
# /dev/harmony at all, so `make -C harmony-linux/linux kernel` is a precondition of
# this image, not an optional refresh).
#
# HOST ORDERING (F10) matters here and is the point of the first run: the Event
# and Entropy doorbell services are offered only once `Vmm::enable_sdk` has been
# called, which `ControlServer::new` does — AFTER `boot_server` has already
# driven the guest to its readiness marker. A probe that fires during that drive
# therefore meets an unwired doorbell and gets `UnknownService` back, which the
# driver turns into a write(2) error and this script reports loudly.
#
# SUCCESS-GATED marker (PR161-F1, the lane's own W1 lesson): BRIDGE_DONE is
# emitted ONLY on a clean probe. A failed probe reboots FIRST, so the marker
# never appears, `drive_to_marker` reaches the triple-fault as a terminal, and
# the gate fails loudly instead of sealing past the failure — the sweep's
# >= 2-distinct-futures check is satisfied by the V-time reseed fold alone, with
# zero guest entropy, so an unconditional marker would print a vacuous PASS.
BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console 2>/dev/null

echo "BRIDGE_LAUNCH: first live /dev/harmony transaction (hm-i8kc F2)"
if [ -e /dev/harmony ]; then
    echo "BRIDGE_DEVNODE: present"
else
    echo "BRIDGE_DEVNODE: ABSENT (kernel lacks CONFIG_HARMONY_DEVICE)"
fi

/opt/harmony/bridge-probe
rc=$?
echo "BRIDGE_PROBE_EXIT=$rc"

if [ "$rc" != "0" ]; then
    echo "BRIDGE_FAILED: reboot (probe failed)"
    exec $BB reboot -f
fi
echo "BRIDGE_DONE"
echo "BRIDGE_CLEAN_TERMINAL: halt"
exec $BB halt -f
