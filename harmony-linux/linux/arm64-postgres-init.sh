#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# M3 outer init. It runs the acceptance PostgreSQL payload in fresh namespaces,
# checks the guest kernel log for the named liveness failures, and emits the
# terminal marker consumed by the host-side report oracle.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mknod -m 0600 /dev/kmsg c 1 11 2>/dev/null
$BB mknod -m 0666 /dev/null c 1 3 2>/dev/null
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mknod -m 0600 /dev/mem c 1 1 2>/dev/null
$BB chmod 0600 /dev/kmsg
$BB chmod 0666 /dev/null
$BB mkdir -p /dev/shm /run /tmp /sys/fs/cgroup
$BB mount -t tmpfs tmpfs /dev/shm
$BB mount -t tmpfs tmpfs /run
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /dev/shm /tmp
exec </dev/null >/dev/kmsg 2>&1

# A dedicated hierarchy witnesses the container boundary; namespaces and the
# chroot are the actual isolation mechanism, so unavailable controllers are not
# silently promoted into a correctness claim.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null || true

echo "DK38: container: unshare(mount,uts,ipc,net,pid) + chroot static postgres rootfs" \
    | /bin/mmio-console
status_file=/run/arm64-postgres.status
if ! ( $BB unshare --mount --uts --ipc --net --pid -f --propagation private \
    "$BB" sh /arm64-postgres-container-setup.sh; \
    printf '%s\n' "$?" >"$status_file" ) 2>&1 | /bin/mmio-console; then
    echo "M3_POSTGRES_FAIL: synchronous oracle transport failed" | /bin/mmio-console
    exec $BB reboot -f
fi
payload_status=$($BB cat "$status_file")
if [ "$payload_status" -ne 0 ]; then
    echo "M3_POSTGRES_FAIL: container payload exited nonzero" | /bin/mmio-console
    exec $BB reboot -f
fi

if $BB dmesg | $BB grep -Eiq 'rcu[^:]*stall|soft lockup|watchdog: BUG'; then
    echo "M3_KERNEL_HEALTH_FAIL" | /bin/mmio-console
    $BB dmesg | $BB grep -Ei 'rcu[^:]*stall|soft lockup|watchdog: BUG' \
        | /bin/mmio-console
    exec $BB reboot -f
fi

printf '%s\n%s\n' "M3_DMESG_OK" "ARM64_PG_M3_READY" | /bin/mmio-console
$BB sync
exec $BB halt -f
