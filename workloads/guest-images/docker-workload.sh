#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run the nested official PostgreSQL OCI container as an outer OCI workload.
# The platform runtime already owns the guest mounts and terminal; this script
# retains only the workload's nested container setup and deterministic oracle.

set -eu

BB=/bin/busybox
BUNDLE=/oci
CONTAINER=pg-container
PGLOG=/run/pg.log
export PATH=/usr/local/bin:/bin:/sbin

log() { $BB echo "DK38: $*"; }

fail() {
    log "FAIL: $*"
    exit 125
}

require_delegated_cgroup() {
    root=/sys/fs/cgroup
    [ -f "$root/cgroup.controllers" ] || fail "platform did not mount cgroup v2"
    [ -f "$root/cgroup.subtree_control" ] || fail "platform cgroup delegation is missing"
    [ -w "$root/cgroup.subtree_control" ] || fail "platform cgroup delegation is read-only"
    [ -d "$root/runtime" ] || fail "platform runtime cgroup is missing"

    self_cgroup=$($BB sed -n 's/^0:://p' /proc/self/cgroup) || \
        fail "cannot read the application cgroup"
    [ "$self_cgroup" = /runtime ] || \
        fail "application is outside the delegated /runtime cgroup: $self_cgroup"
    root_procs=$($BB cat "$root/cgroup.procs") || fail "cannot read delegated cgroup"
    [ -z "$root_procs" ] || fail "delegated cgroup root contains a process"
}

if [ ! -x /usr/local/bin/runc ]; then
    log "FAIL: nested runc is missing"
    exit 127
fi

require_delegated_cgroup

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
