#!/bin/sh
# /init of the **Postgres-in-Docker workload image** (task 38), the runc-direct
# path. Brings up the kernel filesystems + cgroup-v2, runs the **official
# postgres image** as a real OCI container with **`runc`** (the low-level runtime
# dockerd/containerd invoke under the hood), drives the SAME fixed insert/select
# workload as task 37 against the containerized DB over its local unix socket
# (via `runc exec`), streams the container's + the loop's stdout/stderr to ttyS0,
# and reaches a clean deterministic terminal.
#
# **Why runc, not dockerd (the load-bearing finding — see IMPLEMENTATION.md).**
# Under consonance's single-vCPU / V-time model, V-time advances ONLY at VM-exits
# (RDTSC/IO/MMIO). A long-running Go daemon (dockerd, with its containerd)
# busy-spins with no VM-exit while waiting on gRPC, which freezes V-time → the
# LAPIC tick never fires → nothing else is ever scheduled → deadlock (task 37's
# "a spin starves everything; there is no preemption tick"). `runc` sidesteps
# this entirely: it is NOT a daemon — it sets the container up and runs to
# completion (its parent↔init handshake is blocking I/O = a voluntary park, not a
# spin), and the container it runs is the IDENTICAL official-image container
# docker would run. The container's postgres is then a cooperative C workload,
# exactly like task 37.
#
# Two VMM realities (from task 37) shape the control flow:
#   * Never go idle and never busy-spin: V-time freezes both ways. So every wait
#     is COOPERATIVE — a blocking `runc exec` round-trip (the container is doing
#     work = exiting = advancing V-time) or a poll that forks a command each
#     iteration — never a `sleep`, never a `while :; do :; done`.
#   * `poweroff` strands in device_shutdown; we `reboot -f` and the cmdline's
#     `reboot=t,force` makes it a clean triple-fault terminal.

BB=/bin/busybox
export PATH=/usr/local/bin:/bin:/sbin
BUNDLE=/oci
CID=pg
PGLOG=/run/pg.log

log() { $BB echo "DK38: $*"; }
# The container is alive while `runc state` succeeds and is not "stopped" — i.e.
# "created" OR "running" count as alive. (Checking == "running" is a false
# positive: at startup runc briefly reports "created", which would break the
# readiness poll prematurely and run the workload before postgres is up.)
alive() {
    st=$(runc state "$CID" 2>/dev/null) || return 1
    case "$st" in *'"status": "stopped"'*) return 1 ;; *) return 0 ;; esac
}

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

# --- cgroup-v2 (unified) — runc creates the container cgroup under it ----------
# Mount the unified hierarchy, move init out of the root cgroup (so the root has
# no member processes and can delegate controllers), and enable the controllers
# runc needs in the root subtree. cpuset is absent (depends on SMP, off per the
# task-36 audit); runc degrades over it.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for c in cpu io memory pids; do
    $BB echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done
# Private mount propagation (the container stack manages its own mounts). The
# load-bearing initramfs fix is runc `--no-pivot` (baked at /usr/local/bin/runc;
# see build-docker-image.sh): the initramfs root mount has no parent, so runc's
# default pivot_root EINVALs; --no-pivot uses MS_MOVE+chroot, the ramdisk path.
$BB mount --make-rprivate / 2>/dev/null || true

log "runc $(runc --version 2>/dev/null | $BB head -1)"

# --- run the official postgres image as an OCI container ----------------------
# config.json (generated at build time from the image's own runtime config) has
# terminal=false and a fresh empty NETWORK namespace = `--network none`
# (loopback only); the workload reaches postgres over the local unix socket.
# Pipe the container's stdout/stderr through tee → ttyS0 (live, for the gate) and
# → $PGLOG (so we can detect readiness). tee blocks on read = a voluntary park,
# never a spin.
log "runc run --network none (official postgres image)"
cd "$BUNDLE" || { log "FATAL: no $BUNDLE bundle"; $BB sync; exec $BB reboot -f; }
runc run "$CID" 2>&1 | $BB tee "$PGLOG" &
RUNJOB=$!

# Wait for runc to finish CREATING the container before the readiness loops poll
# alive() — otherwise the first `runc state` (cooperative: a forked round-trip
# that advances V-time) can race container creation and `alive` would
# false-FATAL on "does not exist". Under the VMM `runc run` setup is much slower
# than the shell reaching this point, so the race is real here (it was hidden
# under QEMU's faster timing).
until runc state "$CID" >/dev/null 2>&1; do : ; done

# Wait for the REAL server, not the entrypoint's transient init server. The
# official image runs initdb against a temporary unix-socket server, then prints
# "PostgreSQL init process complete; ready for start up." and starts the final
# server. Gate on that marker (PGDATA is fresh each boot, so init always runs),
# then on pg_isready — so we never drive the temp server and race its shutdown.
log "waiting for postgres init to complete"
until $BB grep -q 'init process complete' "$PGLOG" 2>/dev/null; do
    alive || { log "FATAL: container exited during init"; break; }
done
log "waiting for postgres to accept connections"
until runc exec "$CID" pg_isready -U postgres -q >/dev/null 2>&1; do
    alive || { log "FATAL: container exited before ready"; break; }
done
log "postgres ready in container"

# --- drive the SAME insert/select workload as task 37, over the local socket --
# psql runs INSIDE the container (runc exec) connecting to the cluster's unix
# socket; the workload SQL is baked into the container rootfs. Values are a pure
# function of the loop index → the row|… output is a deterministic function of
# the seed (identical rows to task 37's golden).
log "workload begin"
runc exec "$CID" psql -U postgres -d postgres -q -At -F '|' -P pager=off \
    -v ON_ERROR_STOP=1 -f /workload.sql 2>&1
log "workload end"

# --- prove the seeded-CRNG / Go-AT_RANDOM path is deterministic ---------------
# boot_id is the kernel CRNG's own UUID; identical across two same-seed runs is
# the explicit witness that the getrandom/AT_RANDOM path the container stack
# (and initdb's pg_strong_random) seed from is fully on the seeded stream. The
# overall bit-identical serial proves the rest. See IMPLEMENTATION.md.
log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"

# --- cooperative shutdown ----------------------------------------------------
# Stop postgres from INSIDE the container via pg_ctl (exactly like task 37's bare
# Postgres). A host-side `runc kill` can't stop it: postgres is PID 1 of the
# container's pid namespace, and the kernel drops signals sent to a namespace's
# PID 1 from an ANCESTOR namespace. pg_ctl runs *within* the namespace (via
# `runc exec` + gosu→postgres user), so its fast-shutdown signal is delivered and
# handled. `-W` (no-wait) just signals; the shell's `wait "$RUNJOB"` then blocks
# on the `runc run` job so the shutdown checkpoint gets the vCPU and its logs
# ("database system is shut down") stream to ttyS0. `runc delete` clears state.
log "stopping container"
runc exec "$CID" gosu postgres pg_ctl -D /var/lib/postgresql/data -m fast -W stop >/dev/null 2>&1
wait "$RUNJOB" 2>/dev/null
runc delete "$CID" >/dev/null 2>&1
log GUEST_READY
$BB sync

# Force a triple-fault reboot terminal (reboot=t,force) — bypasses the
# device_shutdown stall a plain poweroff hits once block I/O has run.
exec $BB reboot -f
