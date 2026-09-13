#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The acceptance flow, rebuilt as an LSE-only static arm64 payload. This runs
# as the OCI process and deliberately preserves the existing PGC38 markers and
# twenty-row SQL oracle.

PGBIN=/opt/harmony/postgres/bin
PGDATA=/var/lib/postgresql/data
export HOME=/var/lib/postgresql
export LC_ALL=C LANG=C TZ=UTC
export PGUSER=postgres PGHOST=/tmp PGDATABASE=postgres PGTZ=UTC
run_as_postgres() {
    /bin/busybox setuidgid postgres "$@"
}

echo "PGC38: initializing postgres from seeded guest entropy"
run_as_postgres "$PGBIN/initdb" -D "$PGDATA" --no-locale --encoding=UTF8 \
    --auth-local=trust --auth-host=trust -U postgres -N
/bin/busybox cat >>"$PGDATA/postgresql.conf" <<'EOF'

# M3 deterministic static-container overlay.
listen_addresses = ''
unix_socket_directories = '/tmp'
fsync = on
jit = off
log_timezone = 'UTC'
timezone = 'UTC'
log_line_prefix = '[pg %p] '
log_statement = 'none'
shared_buffers = 32MB
max_connections = 16
autovacuum = off
max_wal_size = 64MB
EOF

echo "PGC38: starting postgres in container"
run_as_postgres "$PGBIN/postgres" -D "$PGDATA" &
PGPID=$!

until run_as_postgres "$PGBIN/psql" -q -c 'SELECT 1' >/dev/null 2>&1; do : ; done

echo "PGC38: workload begin"
run_as_postgres "$PGBIN/psql" -q -At -F '|' -P pager=off -v ON_ERROR_STOP=1 -f /workload.sql
echo "PGC38: workload end"

run_as_postgres "$PGBIN/pg_ctl" -D "$PGDATA" -m fast -W stop >/dev/null 2>&1
wait "$PGPID" 2>/dev/null
echo "PGC38: postgres stopped"
echo M3_DMESG_OK
echo ARM64_PG_M3_READY
echo GUEST_READY
