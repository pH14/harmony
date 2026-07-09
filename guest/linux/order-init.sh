#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **bug-2 (ordering/interrupt-timing) benchmark image** (task 69 M2).
# It is the task-37 bare-Postgres init (guest/linux/pg-init.sh) plus the
# planted-bug supervisor `order-super`: bring up the kernel filesystems, run the
# deterministic Postgres insert/select workload to completion (the image's
# determinism pedigree), stop postgres, then run the supervised process whose
# ordering invariant is only violable when an injected interrupt preempts it
# mid-update. A verbatim clone of campaign-init.sh (bug 1) with the
# supervisor/markers swapped. See guest/linux/order-super.c and
# guest/linux/IMPLEMENTATION.md.
#
# The base snapshot is sealed at the `ORDER_READY` marker `order-super` prints
# right before its ordering-sensitive loop (mid-workload, post-readiness). When an
# injected `InjectInterrupt` preempts the process inside its non-atomic update
# window the supervisor prints `ORDER_BUG:` then exits non-zero, which we map to a
# `reboot -f` → `Crash{Shutdown}` (the planted bug — isa-debug-exit is unreachable
# on this kernel, so /init's reboot IS the crash terminal, as for bug 1). With no
# preemption in the window it prints `ORDER_DONE` and we `halt -f` (Quiescent, the
# benign terminal the oracle ignores).
#
# The two consonance-VMM realities from pg-init.sh still hold: no clock-event
# device wakes a blocked nanosleep (readiness/shutdown are awaited
# cooperatively), and `poweroff` strands in device_shutdown once block I/O has
# been used, so we `reboot -f` (a clean triple-fault → KVM_EXIT_SHUTDOWN).

BB=/bin/busybox
PGBIN=/usr/lib/postgresql/17/bin   # tracks PG_MAJOR in versions.lock
PGDATA=/pgmnt/pgdata

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mkdir -p /dev/shm
$BB mount -t tmpfs tmpfs /dev/shm
$BB mount -t tmpfs tmpfs /tmp
$BB mount -t tmpfs tmpfs /run
$BB chmod 1777 /tmp /dev/shm
$BB chmod 0666 /dev/console

export PATH="$PGBIN:/bin"
export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC PGTZ=UTC
export PGUSER=postgres PGHOST=/tmp PGDATABASE=postgres

echo "PGORDER: mounting baked ext4 PGDATA (loop, RAM-backed)"
$BB mount -o loop /pgdata.ext4 /pgmnt
$BB chown 70:70 /tmp /run

echo "PGORDER: starting postgres"
$BB setuidgid postgres postgres -D "$PGDATA" &
PGPID=$!
# Cooperative readiness wait (see pg-init.sh): a blocking psql connect yields the
# single vCPU to the starting postmaster; retry the idempotent probe until it
# succeeds. Output to /dev/null keeps the serial clean.
until $BB setuidgid postgres psql -q -c 'SELECT 1' >/dev/null 2>&1; do : ; done

echo "PGORDER: workload begin"
$BB setuidgid postgres psql -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "PGORDER: workload end"

# Stop postgres cleanly before the supervisor's sensitive loop, so the loop is
# the only activity in the fault window (maximally deterministic — no postgres/
# supervisor interleaving for the fault to have to be robust across).
$BB setuidgid postgres pg_ctl -D "$PGDATA" -m fast -W stop
wait "$PGPID" 2>/dev/null
$BB umount /pgmnt 2>/dev/null

# The planted-bug supervisor, run as root. It prints ORDER_READY (the
# base-snapshot marker), runs the ordering-sensitive loop, and exits 0 on a clean
# run or non-zero when an injected interrupt preempts it mid-update (the planted
# bug). ORDER_DEBUG makes it print the injected vector + crash-channel self-test
# to the boot serial (a deterministic, pre-base-seal diagnostic).
echo "PGORDER: starting the supervised process"
export ORDER_DEBUG=1
/order-super
rc=$?

# The distinctive terminal is the TERMINAL PATH itself, not a userspace port
# write: this kata-derived container kernel has no CONFIG_X86_IOPL_IOPERM /
# CONFIG_DEVPORT, so a guest process cannot reach the isa-debug-exit port (the
# self-test proves all three routes fail). So init maps the outcome to two
# *distinct guest terminals* the kernel can produce:
#   * bug  (rc != 0) -> `reboot -f` -> triple-fault -> KVM_EXIT_SHUTDOWN ->
#     StopReason::Crash{Shutdown}  (the reportable bug),
#   * clean (rc == 0) -> `halt -f`  -> the boot CPU HLTs -> StopReason::Quiescent
#     (the benign terminal the oracle ignores).
# Both use the `-f` (force) path that skips device_shutdown (which strands once
# block I/O has been used — see pg-init.sh). The order oracle keys on
# "a Crash is the bug; Quiescent is clean".
#
# HARDENING (task 69 M2): this crash echo must NOT contain the attribution marker
# substring `ORDER_BUG` — `marker_attributed` scans the whole post-seal console,
# so an init line carrying the marker would let an *unrelated* non-zero exit be
# mis-attributed to this bug. Attribution comes SOLELY from order-super's own
# `ORDER_BUG:` line, printed only on the real trigger. (bug 1's init echo carries
# its marker substring but is shielded by the small-deadline stop landing before
# init runs; this is the strictly-safer form.)
if [ "$rc" != "0" ]; then
    echo "ORDER_ABORT_TERMINAL: reboot (order-super exited $rc)"
    exec $BB reboot -f
fi
echo "ORDER_CLEAN_TERMINAL: halt"
exec $BB halt -f
