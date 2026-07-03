#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **Postgres-campaign workload image** (task 60). It is the task-37
# bare-Postgres init (guest/linux/pg-init.sh) plus the planted-bug supervisor
# `campaign-super`: bring up the kernel filesystems, run the deterministic
# Postgres insert/select workload to completion (the image's determinism
# pedigree), stop postgres, then run the supervised process whose bookkeeping
# invariant is only violable under an injected host fault. See
# guest/linux/campaign-super.c and guest/linux/IMPLEMENTATION.md.
#
# The base snapshot the campaign seals is taken at the `CAMPAIGN_READY` marker
# `campaign-super` prints right before its fault-sensitive loop (mid-workload,
# post-readiness). Under a matching CorruptMemory upset the supervisor aborts via
# isa-debug-exit (Crash{Panic}, the planted bug); with no upset it prints
# `CAMPAIGN_DONE` and we force a reboot (Crash{Shutdown}, the benign terminal the
# oracle ignores).
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

echo "PGCAMPAIGN: mounting baked ext4 PGDATA (loop, RAM-backed)"
$BB mount -o loop /pgdata.ext4 /pgmnt
$BB chown 70:70 /tmp /run

echo "PGCAMPAIGN: starting postgres"
$BB setuidgid postgres postgres -D "$PGDATA" &
PGPID=$!
# Cooperative readiness wait (see pg-init.sh): a blocking psql connect yields the
# single vCPU to the starting postmaster; retry the idempotent probe until it
# succeeds. Output to /dev/null keeps the serial clean.
until $BB setuidgid postgres psql -q -c 'SELECT 1' >/dev/null 2>&1; do : ; done

echo "PGCAMPAIGN: workload begin"
$BB setuidgid postgres psql -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "PGCAMPAIGN: workload end"

# Stop postgres cleanly before the supervisor's sensitive loop, so the loop is
# the only activity in the fault window (maximally deterministic — no postgres/
# supervisor interleaving for the fault to have to be robust across).
$BB setuidgid postgres pg_ctl -D "$PGDATA" -m fast -W stop
wait "$PGPID" 2>/dev/null
$BB umount /pgmnt 2>/dev/null

# The planted-bug supervisor, run as root (CAP_SYS_ADMIN for the CAMPAIGN_DEBUG
# pagemap aid). It prints CAMPAIGN_READY (the base-snapshot marker), runs the
# loop, and exits 0 on a clean run or non-zero when the injected upset trips the
# invariant. CAMPAIGN_DEBUG makes it print the ledger gpa + crash-channel
# self-test to the boot serial (a deterministic, pre-base-seal diagnostic).
echo "PGCAMPAIGN: starting the supervised process"
export CAMPAIGN_DEBUG=1
/campaign-super
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
# block I/O has been used — see pg-init.sh). The campaign oracle keys on
# "a Crash is the bug; Quiescent is clean".
if [ "$rc" != "0" ]; then
    echo "CAMPAIGN_BUG_TERMINAL: reboot (campaign-super exited $rc)"
    exec $BB reboot -f
fi
echo "CAMPAIGN_CLEAN_TERMINAL: halt"
exec $BB halt -f
