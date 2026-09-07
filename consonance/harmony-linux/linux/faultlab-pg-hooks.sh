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
#
# A row drops out of the index only when it is updated and pruned twice: once
# after the build's heap-scan snapshot and before that scan reads its page,
# and again after the validation snapshot and before the validation scan
# reads its page. Each scan lasts a few milliseconds, so the churn keeps
# re-updating a small set of rows spread across the table in short
# transactions, cycling through the set faster than a scan runs and for
# longer than a whole build takes. The loop is the `churn` procedure seeded
# with the cluster: it commits each slice server-side, because a client round
# trip costs a few milliseconds of guest time and would stretch the cycle past
# a scan. The value is rewritten in place at constant width so the versions
# stay HOT. Commits do not wait for the WAL flush: the mechanism depends on
# when a new version becomes visible, and the flush would otherwise dominate
# the cycle time.
hook_churn() {
    rows=$(faultlab_arg faultlab.churn_rows 20)
    slices=$(faultlab_arg faultlab.churn_slices 2)
    rounds=$(faultlab_arg faultlab.churn_rounds 1200)
    $PSQL -c "SET synchronous_commit = off;" -c "CALL churn($rows, $slices, $rounds);" \
        >/dev/null 2>>/run/hook.err || return 1
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
# what a build that skipped HOT-pruned rows fails. Only that finding is the
# bug. pg_amcheck also exits non-zero when the server is down, when a
# connection drops mid-check or when no valid index matches, none of which is
# evidence about the index, so those runs leave the oracle silent.
hook_amcheck() {
    if ! setuidgid 70 "$PGROOT/bin/pg_isready" -h /tmp -U postgres -d faultlab >/dev/null 2>&1; then
        echo "@reachable 26"
        return 0
    fi
    # The output file is private to this instance; several instances of the
    # hook can run at once.
    out=/run/amcheck.$$.out
    setuidgid 70 "$PGROOT/bin/pg_amcheck" -h /tmp -U postgres -d faultlab \
        --heapallindexed --index=cic_k_idx >"$out" 2>&1
    status=$?
    if grep -q "lacks matching index tuple" "$out"; then
        echo "@reachable 24"
        echo "@always 2 0"
    elif [ "$status" -eq 0 ]; then
        echo "@reachable 24"
        echo "@always 2 1"
    else
        echo "@reachable 27"
    fi
    return 0
}

# With autovacuum off this is the one pruning event the search controls, so a
# state where it has run is a coverage goal in its own right.
hook_vacuum() {
    $PSQL -c "VACUUM cic;" >/dev/null 2>>/run/hook.err || return 1
    echo "@reachable 25"
    echo "@sometimes 25"
    return 0
}

case "$1" in
    1) hook_churn ;;
    2) hook_cic ;;
    3) hook_amcheck ;;
    4) hook_vacuum ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
