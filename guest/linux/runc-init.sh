#!/bin/sh
# /init of the **Postgres-via-real-`runc` workload image** (task 48). Selected by
# the kernel `rdinit=/runc-init` cmdline param; the task-38 `unshare` path stays
# baked as the default `/init` (`docker-init.sh`) for comparison. Brings up the
# kernel filesystems + cgroup-v2, then runs the **official postgres OCI image** as
# a real container with the **actual `runc` binary** (`runc run`) — NOT the task-38
# `unshare`/`chroot`/`setpriv` shim — and waits while the container drives the
# task-42 `gen_random_uuid()`/`clock_timestamp()` workload over its local unix
# socket, streaming its stdout/stderr to ttyS0, to a clean terminal.
#
# **Why this works now where task 38 had to use `unshare` (the unlock — see
# guest/linux/IMPLEMENTATION.md + tasks/47-deterministic-preemption-timer.md).**
# `runc`/its Go container-init busy-spin (`procyield`/`osyield`) with no natural
# VM-exit; under task 38's single-vCPU / V-time model that froze V-time → the LAPIC
# tick never fired → the Go scheduler never ran → the container reached "created"
# but its init never execed the command (a deadlock). Task 47 made the V-time LAPIC
# timer **preempt** a busy-spinning thread at the seed-deterministic V-time deadline
# (`run_until` = PMU overflow + single-step to the exact retired-branch count), which
# the VMM run-loop now drives automatically on the patched Linux boot. So the Go
# runtime is preempted on time, the scheduler runs, the create→exec handshake
# completes, and the **real `runc`** runs the container — deterministically, because
# the preemption instant is a pure function of the seed.
#
# Two VMM realities (from task 37/38) still shape the control flow:
#   * Never go idle on a blocking wait that needs a wakeup the VMM won't deliver
#     and never busy-spin without RDTSC. The container is the only runnable work and
#     is busy throughout (runc setup → postgres + the cooperative psql loop); the
#     init just `wait`s on `runc run`, which blocks in waitpid until the container
#     exits — no host-side poll to freeze on.
#   * `poweroff` strands in device_shutdown; we `reboot -f` and the cmdline's
#     `reboot=t,force` makes it a clean triple-fault terminal.

BB=/bin/busybox
export PATH=/usr/local/bin:/bin:/sbin
BUNDLE=/oci
CONTAINER=pg-container

log() { $BB echo "RUNC48: $*"; }

# --- kernel filesystems (identical to docker-init.sh) ------------------------
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

# --- cgroup-v2 (unified) — runc creates the container's own cgroup --------------
# Mount the unified hierarchy, move init out of the root cgroup into a leaf (so the
# root has no member processes and can delegate controllers), and enable the
# controllers in the root subtree. runc (cgroupfs driver, the default with no
# systemd) then creates `/sys/fs/cgroup/<cgroupsPath>` (config.json's
# `linux.cgroupsPath = pg-container`) ITSELF and places the container there — we do
# NOT create it here (unlike docker-init.sh, where the unshared container inherited
# init's cgroup). cpuset is absent (depends on SMP, off per the task-36 audit); the
# others give real cgroup isolation.
$BB mount -t cgroup2 none /sys/fs/cgroup 2>/dev/null
$BB mkdir -p /sys/fs/cgroup/init
$BB echo $$ > /sys/fs/cgroup/init/cgroup.procs 2>/dev/null || true
for c in cpu io memory pids; do
    $BB echo "+$c" > /sys/fs/cgroup/cgroup.subtree_control 2>/dev/null || true
done
$BB mount --make-rprivate / 2>/dev/null || true

log "OCI runtime: real runc $(runc --version 2>/dev/null | $BB head -1)"

# --- run the official postgres OCI image with the REAL runc (no unshare shim) ----
# `runc run <id>` = create + start in one: it sets up the namespaces (the bundle's
# config.json declares an empty NETWORK ns = `--network none`, plus mount/pid/ipc/
# uts), the per-container cgroup, the device-cgroup (config.json allows all devices —
# the bare `runc spec` default-deny eBPF filter kills PID 1 at exec on this kernel),
# applies the seccomp profile (CONFIG_SECCOMP_FILTER is on in the Kata base), and
# execs the container's `/run-workload.sh` (pg-container-run.sh): start postgres →
# the cooperative psql readiness loop → the task-42 UUID/time workload → cooperative
# shutdown. `--bundle /oci` points at the baked bundle; the `runc` on PATH is the
# `--no-pivot` wrapper (the rootfs sits on the initramfs ramdisk, whose root mount
# has no parent, so runc's default pivot_root EINVALs — `--no-pivot` switches to the
# MS_MOVE+chroot path documented for exactly that). `terminal=false` in config.json
# makes runc inherit OUR stdio for the container, so postgres' logs + the workload's
# `row|i|count|sum|uuid|t` lines stream straight to ttyS0 (the gate serial).
#
# Foreground: `runc run` blocks in waitpid until the container exits, so the single
# vCPU runs the container (whose RDTSC/cooperative yields advance V-time, and whose
# Go-runtime spins are now preempted by the V-time LAPIC timer) — no host-side idle.
log "launching the official postgres OCI container via REAL runc: runc run $CONTAINER"
runc run --bundle "$BUNDLE" "$CONTAINER"
RC=$?
log "runc run exited rc=$RC"

# Prove the seeded-CRNG path is deterministic: boot_id is the kernel CRNG's own
# UUID; identical across two same-seed runs witnesses that getrandom/AT_RANDOM is
# on the seeded stream. The overall bit-identical serial proves the rest.
log "boot_id=$($BB cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
log GUEST_READY
$BB sync

# Force a triple-fault reboot terminal (reboot=t,force) — bypasses the
# device_shutdown stall a plain poweroff hits once block I/O has run.
exec $BB reboot -f
