#!/bin/sh
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

# The planted-bug supervisor, run as root (ioperm for the isa-debug-exit crash
# channel needs CAP_SYS_RAWIO; the CAMPAIGN_DEBUG pagemap aid needs CAP_SYS_ADMIN).
# It prints CAMPAIGN_READY (the base-snapshot marker), runs the loop, and either
# aborts via isa-debug-exit (the bug) or prints CAMPAIGN_DONE.
echo "PGCAMPAIGN: starting the supervised process"
/campaign-super
rc=$?

# If campaign-super returned (no upset, or the ioperm fallback fired), force the
# reboot. A nonzero rc from the ioperm fallback is surfaced first so the operator
# still sees the bug on the serial even where isa-debug-exit was unavailable.
if [ "$rc" != "0" ]; then
    echo "CAMPAIGN_BUG_FALLBACK: campaign-super exited $rc (isa-debug-exit unavailable?)"
fi
exec $BB reboot -f
