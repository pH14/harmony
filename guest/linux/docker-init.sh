#!/bin/sh
# /init of the **Postgres-as-an-OCI-container workload image** (task 38). Brings
# up the kernel filesystems + cgroup-v2, then runs the **official postgres OCI
# image** as a real container — `unshare`d mount/uts/ipc/net/pid namespaces +
# chroot into the image rootfs + a per-container cgroup — and waits while the
# container drives the SAME fixed insert/select workload as task 37 over its
# local unix socket, streaming its stdout/stderr to ttyS0, to a clean terminal.
#
# **Why unshare, not runc/dockerd (the load-bearing finding — see
# IMPLEMENTATION.md).** Under consonance's single-vCPU / V-time model, V-time
# advances ONLY when the guest executes RDTSC/RDMSR(IA32_TSC). Any busy-wait that
# does no RDTSC freezes V-time → the LAPIC tick never fires → nothing is ever
# preempted → deadlock. **dockerd** busy-spins on gRPC (frozen at "containerd
# successfully booted"); **runc** itself deadlocks its container-init — the
# container reaches "created" but its Go init never execs the command (verified
# with a trivial `/bin/sh -c echo`). So we build the container with `unshare` +
# `chroot` + `setpriv` (plain syscalls, no Go init, no exec-fifo) and run the
# task-37 cooperative flow INSIDE it; that flow advances V-time the way task 37
# did (a blocking `psql` connect yields the vCPU to the starting postmaster,
# whose RDTSCs trap → V-time advances → the tick fires → postgres is scheduled).
# The full Docker/runc stack stays baked — present, but it can't run here.
#
# Two VMM realities (from task 37) shape the control flow:
#   * Never go idle and never busy-spin without RDTSC: V-time freezes both ways.
#     The container is the only runnable work and is busy throughout (postgres +
#     the psql loop), so the init just `wait`s — no host-side poll to freeze on.
#   * `poweroff` strands in device_shutdown; we `reboot -f` and the cmdline's
#     `reboot=t,force` makes it a clean triple-fault terminal.

BB=/bin/busybox
export PATH=/usr/local/bin:/bin:/sbin
PGLOG=/run/pg.log

log() { $BB echo "DK38: $*"; }

# --- kernel filesystems ------------------------------------------------------
$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mkdir -p /dev/shm /dev/pts /run /tmp
$BB mount -t tmpfs tmpfs /dev/shm
$BB mount -t devpts devpts /dev/pts 2>/dev/null
$BB mount -t tmpfs tmpfs /run
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp /dev/shm
$BB chmod 0666 /dev/console      # let the container reopen the console

# --- cgroup-v2 (unified) — the container runs in its own cgroup -----------------
# Mount the unified hierarchy, move init out of the root cgroup (so the root can
# delegate controllers), enable the controllers in the root subtree, then create
# the container's own cgroup and move init into it — the container the init forks
# (via unshare) inherits it. cpuset is absent (depends on SMP, off per the task-36
# audit); the others give real cgroup isolation.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for c in cpu io memory pids; do
    $BB echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done
$BB mkdir -p /sys/fs/cgroup/pg-container
$BB echo $$ > /sys/fs/cgroup/pg-container/cgroup.procs 2>/dev/null || true
$BB mount --make-rprivate / 2>/dev/null || true

log "OCI runtime baked: runc $(runc --version 2>/dev/null | $BB head -1) (unused — deadlocks under the VMM)"

# --- run the official postgres OCI image in a real container (unshare, not runc) -
# We containerize the official postgres image with namespaces built directly via
# `unshare` + chroot, NOT runc: runc's container-init (Go) DEADLOCKS under the
# consonance VMM — it reaches "created" but never execs the command (verified with
# even a trivial `/bin/sh -c echo`; the Go create→exec/exec-fifo handshake needs a
# free-running clock the work-driven V-time model doesn't provide). `unshare`/
# `mount`/`chroot`/`setpriv` are plain syscalls, and the container then runs the
# cooperative task-37 flow (container-setup.sh → chroot → /run-workload.sh:
# postgres + the psql loop), which advances V-time exactly as task 37's bare
# Postgres did. The full Docker/runc stack stays baked (the OCI runtime is
# present, it just can't run here) — see guest/linux/IMPLEMENTATION.md.
#
# Namespaces: --mount (isolated mounts + chroot to the image rootfs), --pid (own
# PID space; -f forks so the container is PID 1), --net (= `--network none`:
# loopback only, no veth), --uts, --ipc. The container's stdout/stderr (postgres'
# logs + the workload's row|… output) stream through tee → ttyS0 (the gate serial).
log "container: unshare(mount,uts,ipc,net,pid) + chroot the official postgres image rootfs"
$BB unshare --mount --uts --ipc --net --pid -f --propagation private \
    "$BB" sh /container-setup.sh 2>&1 | $BB tee "$PGLOG" &
RUNJOB=$!

# The container is the only runnable work; the init parks here on `wait` and the
# vCPU runs the container, whose cooperative postgres flow advances V-time. When
# its script finishes (workload + clean shutdown), `unshare` returns, tee EOFs,
# and `wait` returns. No idle-HLT — the container is busy until postgres stops.
wait "$RUNJOB" 2>/dev/null

# Prove the seeded-CRNG path is deterministic: boot_id is the kernel CRNG's own
# UUID; identical across two same-seed runs witnesses that getrandom/AT_RANDOM is
# on the seeded stream. The overall bit-identical serial proves the rest.
log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
log GUEST_READY
$BB sync

# Force a triple-fault reboot terminal (reboot=t,force) — bypasses the
# device_shutdown stall a plain poweroff hits once block I/O has run.
exec $BB reboot -f
