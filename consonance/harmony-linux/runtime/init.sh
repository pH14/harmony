#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later

set -eu

BUSYBOX=/bin/busybox
RUNC=/usr/bin/runc
BUNDLE=/harmony-oci
CONTAINER_ID=harmony

log() {
    printf '%s\n' "$*"
}

finish() {
    status=$1
    log "HARMONY_OCI_EXIT rc=$status"
    if [ -n "${HARMONY_CONSOLE_PID:-}" ]; then
        exec >/dev/null 2>&1
        wait "$HARMONY_CONSOLE_PID" || status=125
    fi
    "$BUSYBOX" reboot -f 2>/dev/null || true
    exit "$status"
}

startup_failure() {
    status=$1
    log "HARMONY_OCI_STARTUP_EXIT rc=$status"
    finish "$status"
}

mounted() {
    "$BUSYBOX" grep -qs " $1 " /proc/mounts
}

mount_required() {
    filesystem=$1
    source=$2
    target=$3
    "$BUSYBOX" mkdir -p "$target" || startup_failure 125
    if "$BUSYBOX" mount -t "$filesystem" "$source" "$target" 2>/dev/null; then
        return 0
    fi
    mounted "$target" || startup_failure 125
}

log "HARMONY_OCI: startup"

[ -x "$BUSYBOX" ] || finish 127
[ -x "$RUNC" ] || startup_failure 127
[ -d "$BUNDLE" ] || startup_failure 125
[ -f "$BUNDLE/config.json" ] || startup_failure 125
[ -f "$BUNDLE/execution.json" ] || startup_failure 125
[ -d "$BUNDLE/rootfs" ] || startup_failure 125
[ -x /usr/lib/harmony/supervisor ] || startup_failure 127

mount_required proc proc /proc
mount_required sysfs sysfs /sys
mount_required devtmpfs dev /dev
mount_required devpts devpts /dev/pts
mount_required tmpfs tmpfs /dev/shm
mount_required tmpfs tmpfs /run
mount_required tmpfs tmpfs /tmp
mount_required cgroup2 none /sys/fs/cgroup

"$BUSYBOX" mount --make-rprivate / 2>/dev/null || startup_failure 125
[ -e /dev/harmony ] || startup_failure 125
[ -e /dev/harmony-park ] || startup_failure 125

runc_pid=
# shellcheck disable=SC2329
forward_term() {
    if [ -n "$runc_pid" ]; then
        "$BUSYBOX" kill -TERM "$runc_pid" 2>/dev/null || true
    fi
}

# shellcheck disable=SC2329
forward_int() {
    if [ -n "$runc_pid" ]; then
        "$BUSYBOX" kill -INT "$runc_pid" 2>/dev/null || true
    fi
}

# shellcheck disable=SC2329
forward_hup() {
    if [ -n "$runc_pid" ]; then
        "$BUSYBOX" kill -HUP "$runc_pid" 2>/dev/null || true
    fi
}

trap forward_term TERM
trap forward_int INT
trap forward_hup HUP

log "HARMONY_OCI: runc"
/usr/bin/runc run --no-pivot --bundle "$BUNDLE" "$CONTAINER_ID" &
runc_pid=$!

if wait "$runc_pid"; then
    runc_status=0
else
    runc_status=$?
fi

trap - TERM INT HUP
log "HARMONY_OCI_APP_EXIT rc=$runc_status"
finish "$runc_status"
