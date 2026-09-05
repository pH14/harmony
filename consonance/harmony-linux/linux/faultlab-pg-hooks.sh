#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hook dispatcher for the PostgreSQL CREATE INDEX CONCURRENTLY bundle. One
# argument: the hook id from the bundle file. Directives go to stdout,
# diagnostics to stderr.
set -u
. /faultlab-common.sh

PSQL="setuidgid 70 $PGROOT/bin/psql -h /tmp -U postgres -d faultlab -v ON_ERROR_STOP=1 -qtAX"

# The churn column is deliberately not the indexed one: an UPDATE that touches
# no indexed column is eligible for a HOT update, and HOT pruning of those
# tuples during a concurrent build is the mechanism the 14.3 bug turns on.
hook_churn() {
    rows=$(faultlab_arg faultlab.churn_rows 2000)
    $PSQL -c "UPDATE cic SET pad = pad || '.' WHERE id <= $rows;" >/dev/null 2>>/run/hook.err || return 1
    echo "@reachable 20"
    echo "@sometimes 21"
    return 0
}

hook_cic() {
    $PSQL -c "DROP INDEX IF EXISTS cic_k_idx;" >/dev/null 2>>/run/hook.err || return 1
    $PSQL -c "CREATE INDEX CONCURRENTLY cic_k_idx ON cic(k);" >/dev/null 2>>/run/hook.err || return 1
    echo "@reachable 22"
    echo "@sometimes 23"
    return 0
}

# pg_amcheck --heapallindexed is the official detector for this corruption: it
# verifies every live heap tuple has a matching index entry, which is exactly
# what a build that skipped HOT-pruned rows fails.
hook_amcheck() {
    if setuidgid 70 "$PGROOT/bin/pg_amcheck" -h /tmp -U postgres -d faultlab \
        --heapallindexed --index=cic_k_idx >/run/amcheck.out 2>&1; then
        echo "@reachable 24"
        echo "@always 2 1"
    else
        echo "@always 2 0"
    fi
    return 0
}

hook_vacuum() {
    $PSQL -c "VACUUM cic;" >/dev/null 2>>/run/hook.err || return 1
    echo "@reachable 25"
    return 0
}

case "$1" in
    1) hook_churn ;;
    2) hook_cic ;;
    3) hook_amcheck ;;
    4) hook_vacuum ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
