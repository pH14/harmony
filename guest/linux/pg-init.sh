#!/bin/sh
# /init of the **bare-Postgres workload image** (task 37). Brings up the kernel
# filesystems, loop-mounts the RAM-backed ext4 holding the pre-`initdb`'d PGDATA,
# starts a real PostgreSQL server, drives a fixed insert/select workload loop, and
# reaches a clean deterministic terminal. Every byte it prints to ttyS0 (postgres'
# own stdout/stderr plus the per-iteration query results) is part of the
# deterministic-twice golden. The workload (task 42) deliberately populates each row
# with values that *look* nondeterministic — a gen_random_uuid() id and a
# clock_timestamp() wall-clock column — to prove they come out bit-identical anyway:
# gen_random_uuid() rides pg_strong_random -> the seeded CRNG, and the clock is
# V-time-driven; the running count/sum stays a pure function of the loop index (the
# gate's deterministic anchor) and locale/TZ are pinned so the uuid/timestamp text
# renders stably. Determinism of the *execution* (TSC, RNG, fork order, the clock) is
# enforced from below by the patched KVM backend + V-time — see
# guest/linux/IMPLEMENTATION.md.
#
# Two consonance-VMM realities shape the control flow (see IMPLEMENTATION.md):
#   * The VMM terminates the run on the first guest HLT and does not wake a
#     blocked `nanosleep` (no clock-event device is set up). So readiness/shutdown
#     are awaited COOPERATIVELY — a blocking psql connect / the shell's `wait`
#     yield the single vCPU to postgres — never by `sleep`-polling (the sleeper
#     never wakes) and never by a busy spin (it would starve postgres, no preempt).
#   * `poweroff` strands in the kernel's device_shutdown once block I/O has been
#     used. We unmount the ext4 and `reboot -f`; the cmdline's `reboot=t,force`
#     turns that into a triple-fault → a clean KVM_EXIT_SHUTDOWN terminal.

BB=/bin/busybox
PGBIN=/usr/lib/postgresql/17/bin   # tracks PG_MAJOR in versions.lock
PGDATA=/pgmnt/pgdata

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
# DEVTMPFS_MOUNT already gives us /dev; mount it explicitly in case it is off.
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mkdir -p /dev/shm
$BB mount -t tmpfs tmpfs /dev/shm          # POSIX shared memory for postgres
$BB mount -t tmpfs tmpfs /tmp              # unix socket dir + scratch
$BB mount -t tmpfs tmpfs /run
$BB chmod 1777 /tmp /dev/shm
# postgres (uid 70) inherits init's already-open console fd for its logs; the
# chmod lets it (and any child) reopen /dev/console for writing too.
$BB chmod 0666 /dev/console

export PATH="$PGBIN:/bin"
export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC PGTZ=UTC
export PGUSER=postgres PGHOST=/tmp PGDATABASE=postgres

echo "PG37: mounting baked ext4 PGDATA (loop, RAM-backed)"
$BB mount -o loop /pgdata.ext4 /pgmnt
$BB chown 70:70 /tmp /run

echo "PG37: starting postgres"
$BB setuidgid postgres postgres -D "$PGDATA" &
PGPID=$!
# Cooperative readiness wait: each psql connect blocks, yielding the single vCPU
# to the starting postmaster. Retry the idempotent SELECT 1 until it succeeds;
# probe output to /dev/null keeps the golden clean. The retry count is a
# deterministic function of the V-time postgres takes to reach PM_RUN.
until $BB setuidgid postgres psql -q -c 'SELECT 1' >/dev/null 2>&1; do : ; done

echo "PG37: workload begin"
$BB setuidgid postgres psql -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "PG37: workload end"

# Cooperative shutdown: send the fast-shutdown signal without waiting, then BLOCK
# on the background postmaster via the shell's `wait` builtin so postgres gets the
# vCPU to run its shutdown checkpoint (a sleep/spin poll would hang or starve it).
$BB setuidgid postgres pg_ctl -D "$PGDATA" -m fast -W stop
wait "$PGPID" 2>/dev/null
echo GUEST_READY

# Unmount the ext4 (auto-detaches loop0 via mount -o loop's autoclear) and force a
# triple-fault reboot (reboot=t,force) — a clean terminal that bypasses the
# device_shutdown stall a plain poweroff hits here.
$BB umount /pgmnt 2>/dev/null
exec $BB reboot -f
