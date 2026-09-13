#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run the PostgreSQL workload from an OCI process. Platform PID 1 owns mounts,
# signals, and termination; this script owns only the application flow.

set -u

BB=/bin/busybox
PGBIN=/usr/lib/postgresql/17/bin
PGDATA=/var/lib/postgresql/data
variant=${HARMONY_POSTGRES_VARIANT:-postgres}
supervisor=
prefix=

case "$variant" in
    postgres)
        prefix=PG37
        ;;
    campaign)
        prefix=PGCAMPAIGN
        supervisor=/campaign-super
        export CAMPAIGN_DEBUG=1
        ;;
    order)
        prefix=PGORDER
        supervisor=/order-super
        export ORDER_DEBUG=1
        ;;
    uuid)
        prefix=PGUUID
        supervisor=/uuid-super
        export UUID_DEBUG=1
        ;;
    *)
        echo "unknown PostgreSQL workload variant: $variant" >&2
        exit 2
        ;;
esac

export PATH="$PGBIN:/bin"
export HOME=/var/lib/postgresql
export LC_ALL=C.UTF-8 LANG=C.UTF-8 TZ=UTC PGTZ=UTC
export PGUSER=postgres PGHOST=/tmp PGDATABASE=postgres

# The platform mounts these paths for every OCI process. Keep the application
# writable where PostgreSQL creates its socket and POSIX shared memory.
$BB mkdir -p /tmp /dev/shm
$BB chmod 1777 /tmp /dev/shm 2>/dev/null || true

echo "$prefix: starting postgres"
$BB setuidgid postgres "$PGBIN/postgres" -D "$PGDATA" &
PGPID=$!

# A blocking connection gives the starting postmaster the vCPU under the
# deterministic guest model. Avoid a timer-dependent sleep or a host-side poll.
until $BB setuidgid postgres "$PGBIN/psql" -q -c 'SELECT 1' >/dev/null 2>&1; do
    :
done

echo "$prefix: workload begin"
$BB setuidgid postgres "$PGBIN/psql" -q -At -F '|' -P pager=off \
    -v ON_ERROR_STOP=1 -f /workload.sql
echo "$prefix: workload end"

# Stop PostgreSQL before an optional planted-bug supervisor so its fault window
# has the same application ordering as the previous guest image.
$BB setuidgid postgres "$PGBIN/pg_ctl" -D "$PGDATA" -m fast -W stop \
    >/dev/null 2>&1
wait "$PGPID" 2>/dev/null || true

if [ -n "$supervisor" ]; then
    echo "$prefix: starting the supervised process"
    "$supervisor"
    status=$?
    if [ "$status" -eq 0 ]; then
        case "$variant" in
            campaign) echo "CAMPAIGN_CLEAN_TERMINAL: application exit" ;;
            order) echo "ORDER_CLEAN_TERMINAL: application exit" ;;
            uuid) echo "UUID_CLEAN_TERMINAL: application exit" ;;
        esac
    else
        case "$variant" in
            campaign) echo "CAMPAIGN_BUG_TERMINAL: application exit rc=$status" ;;
            order) echo "ORDER_ABORT_TERMINAL: application exit rc=$status" ;;
            uuid) echo "UUID_ABORT_TERMINAL: application exit rc=$status" ;;
        esac
    fi
    exit "$status"
fi

echo GUEST_READY
exit 0
