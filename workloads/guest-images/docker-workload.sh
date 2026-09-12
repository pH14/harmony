#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run the nested official PostgreSQL OCI container as an outer OCI workload.
# The platform runtime already owns the guest mounts and terminal; this script
# retains only the workload's nested container setup and deterministic oracle.

set -u

BB=/bin/busybox
BUNDLE=/oci
CONTAINER=pg-container
PGLOG=/run/pg.log
export PATH=/usr/local/bin:/bin:/sbin

log() { $BB echo "DK38: $*"; }

if [ ! -x /usr/local/bin/runc ]; then
    log "FAIL: nested runc is missing"
    exit 127
fi

# The outer platform mount namespace is already established. Nested runc may
# still need a cgroup hierarchy for its child container; this is an optional
# kernel facility for this workload and its absence is reported by runc.
$BB mkdir -p /sys/fs/cgroup
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null || true
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for controller in cpu io memory pids; do
    $BB echo "+$controller" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done
$BB mount --make-rprivate / 2>/dev/null || true

log "OCI workload: nested runc $(runc --version 2>/dev/null | $BB head -1)"
log "launching official postgres OCI container via nested runc"
if runc run --bundle "$BUNDLE" "$CONTAINER" >"$PGLOG" 2>&1; then
    status=0
else
    status=$?
fi
$BB cat "$PGLOG"

log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
if [ "$status" -eq 0 ]; then
    log GUEST_READY
fi
exit "$status"
