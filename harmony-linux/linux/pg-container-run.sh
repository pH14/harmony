#!/bin/sh
# Runs as PID 1 INSIDE the official-postgres OCI container (its `process.args`).
# The ENTIRE task-37-style workload flow lives here — start the postgres binary,
# wait for it cooperatively, drive the fixed insert/select loop, stop it — so the
# guest `/init` only has to `runc run` this and wait. The point: the cooperative
# `psql` loop runs *inside* the container, where it works under the consonance
# VMM for exactly the same reason task 37's bare-Postgres loop did — a blocking
# `psql` connect yields the single vCPU to the starting postmaster, whose RDTSCs
# (timestamps) trap → VM-exits → V-time advances → the periodic tick fires →
# postgres is scheduled and reaches "ready". Driving postgres from *outside* the
# container (guest init → `runc exec`/grep) cannot do this: those host-side ops
# either hang (runc exec) or are passive syscalls that cause no VM-exit (grep),
# freezing V-time. So the driving must live next to postgres, in here.
set -u
PGBIN=/usr/lib/postgresql/17/bin
PGDATA=/var/lib/postgresql/data
export PGUSER=postgres PGHOST=/run/postgresql PGDATABASE=postgres PGTZ=UTC LC_ALL=C.UTF-8

echo "PGC38: starting postgres in container"
"$PGBIN/postgres" -D "$PGDATA" &
PGPID=$!

# Cooperative readiness wait (task 37): each blocking `psql` connect yields the
# vCPU to the starting postmaster; retry the idempotent SELECT 1 until it
# connects. Never a `sleep` (the sleeper never wakes), never a busy spin.
until "$PGBIN/psql" -q -c 'SELECT 1' >/dev/null 2>&1; do : ; done

echo "PGC38: workload begin"
"$PGBIN/psql" -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "PGC38: workload end"

# Cooperative shutdown: fast-shutdown signal (postmaster is this script's child,
# so the signal is delivered normally — no namespace-PID-1 issue), then BLOCK on
# the postmaster via `wait` so its shutdown checkpoint gets the vCPU.
"$PGBIN/pg_ctl" -D "$PGDATA" -m fast -W stop >/dev/null 2>&1
wait "$PGPID" 2>/dev/null
echo "PGC38: postgres stopped"
