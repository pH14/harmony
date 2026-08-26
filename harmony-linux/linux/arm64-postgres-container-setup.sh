#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# PID 1 in M3's fresh mount/UTS/IPC/network/PID namespaces. Build the isolated
# filesystem view, then enter the static PostgreSQL root as uid 65534.

BB=/bin/busybox
R=/container

$BB mount -t proc proc "$R/proc"
$BB mount --rbind /dev "$R/dev"
$BB mount -t tmpfs tmpfs "$R/dev/shm"
$BB chmod 1777 "$R/dev/shm"
$BB mount -t tmpfs tmpfs "$R/tmp"
$BB chmod 1777 "$R/tmp"

exec $BB chroot "$R" /bin/busybox setuidgid postgres \
    /bin/sh /run-workload.sh
