#!/bin/sh
# Runs as PID 1 of fresh mount/uts/ipc/net/pid namespaces (the guest `/init`
# `unshare`s into them). Sets up the container's filesystem view, chroots into
# the official postgres OCI image's rootfs, drops to the postgres uid, and execs
# the in-container workload flow (`/run-workload.sh`).
#
# This builds a real OCI-image container — namespace + cgroup isolation of the
# official postgres image — WITHOUT runc, on purpose: runc's container-init (Go)
# deadlocks under the consonance VMM (it reaches "created" but never execs the
# command — a Go/exec-fifo handshake that needs a free-running clock the V-time
# model doesn't provide). `unshare`/`mount`/`chroot`/`setpriv` are plain
# syscalls, and once postgres is running its cooperative psql loop advances
# V-time exactly as task 37's did. See harmony-linux/linux/IMPLEMENTATION.md.
#
# `--network none`: the fresh net namespace has only loopback and no veth, so the
# container has no external connectivity; the workload reaches postgres over the
# container-local unix socket.
BB=/bin/busybox
R=/oci/rootfs

$BB mount --make-rprivate /
# The container's own /proc (shows the new PID namespace), working device nodes
# (rbind the host devtmpfs — a nodev tmpfs can't host mknod'd nodes), a fresh
# /dev/shm for postgres' POSIX shared memory, and a scratch /tmp.
$BB mount -t proc proc "$R/proc"
$BB mount --rbind /dev "$R/dev"
$BB mount -t tmpfs tmpfs "$R/dev/shm"
$BB chmod 1777 "$R/dev/shm"
$BB mount -t tmpfs tmpfs "$R/tmp"
$BB chmod 1777 "$R/tmp"

# chroot into the image rootfs, drop to the postgres user (uid 999) with the
# image's own `setpriv` (C, no /proc/self/exe dependency), and run the workload.
exec $BB chroot "$R" /usr/bin/setpriv --reuid 999 --regid 999 --clear-groups \
    /bin/sh /run-workload.sh
