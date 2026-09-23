#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hook dispatcher for the CREATE INDEX CONCURRENTLY workload. One argument: the
# hook id from /etc/harmony/bundle. Assertions go to the Antithesis SDK sink,
# diagnostics to /run/hook.err.
set -u

PGBIN=/usr/lib/postgresql/bin

# faultlab_arg <key> <default>: read `key=value` from the kernel command line.
# The command line is the only channel a campaign has for turning image knobs
# without rebuilding, so every tunable a run may vary goes through it.
# /proc/cmdline is read per call because each hook runs as its own process.
faultlab_arg() {
    # shellcheck disable=SC2013  # the command line is whitespace-separated words
    _value=$(for _word in $(cat /proc/cmdline 2>/dev/null); do
        case "$_word" in "$1"=*) echo "${_word#*=}" ;; esac
    done | tail -1)
    if [ -z "$_value" ]; then echo "$2"; else echo "$_value"; fi
}

CHURN_FINISHED="churn finished"
BUILD_FINISHED="concurrent index build finished"
VACUUM_FINISHED="vacuum finished"
AMCHECK_COMPARED="amcheck compared the index"
AMCHECK_SERVER_DOWN="amcheck found the server down"
AMCHECK_FAILED="amcheck failed without a finding"
INDEX_COMPLETE="every heap tuple has an index entry"

# antithesis_assert <type> <id> <hit> <condition>: one record in the Antithesis
# fallback SDK format. A single printf writes the whole line in one write call.
antithesis_assert() {
    [ -n "${ANTITHESIS_OUTPUT_DIR:-}" ] || return 0
    case "$1" in always) _display=Always ;; *) _display=Reachable ;; esac
    printf '{"antithesis_assert":{"hit":%s,"must_hit":true,"assert_type":"%s","display_type":"%s","message":"%s","condition":%s,"id":"%s","location":{"class":"postgres-hooks","function":"","file":"hooks.sh","begin_line":0,"begin_column":0},"details":null}}\n' \
        "$3" "$1" "$_display" "$2" "$4" "$2" >>"$ANTITHESIS_OUTPUT_DIR/sdk.jsonl"
}

reached() {
    antithesis_assert reachability "$1" true true
}

declare_assertions() {
    for _id in "$CHURN_FINISHED" "$BUILD_FINISHED" "$VACUUM_FINISHED" \
        "$AMCHECK_COMPARED" "$AMCHECK_SERVER_DOWN" "$AMCHECK_FAILED"; do
        antithesis_assert reachability "$_id" false false
    done
    antithesis_assert always "$INDEX_COMPLETE" false false
}

as_postgres() {
    setpriv --reuid=70 --regid=70 --clear-groups "$@"
}

psql_faultlab() {
    as_postgres "$PGBIN/psql" -h /tmp -U postgres -d faultlab \
        -v ON_ERROR_STOP=1 -qtAX "$@"
}

# The churn column is deliberately not the indexed one: an UPDATE that touches
# no indexed column is eligible for a HOT update, and HOT pruning of those
# tuples during a concurrent build is the mechanism the 14.3 bug turns on.
#
# A row drops out of the index only when it is updated and pruned twice: once
# after the build's heap-scan snapshot and before that scan reads its page, and
# again after the validation snapshot and before the validation scan reads its
# page. Each scan lasts a few milliseconds, so the churn keeps re-updating a
# small set of rows spread across the table in short transactions, cycling
# through the set faster than a scan runs and for longer than a whole build
# takes. Commits do not wait for the WAL flush: the mechanism depends on when a
# new version becomes visible, and the flush would otherwise dominate the cycle.
hook_churn() {
    rows=$(faultlab_arg faultlab.churn_rows 20)
    slices=$(faultlab_arg faultlab.churn_slices 2)
    rounds=$(faultlab_arg faultlab.churn_rounds 1200)
    psql_faultlab -c "SET synchronous_commit = off;" \
        -c "CALL churn($rows, $slices, $rounds);" \
        >/dev/null 2>>/run/hook.err || return 1
    reached "$CHURN_FINISHED"
    return 0
}

hook_cic() {
    psql_faultlab -c "DROP INDEX IF EXISTS cic_k_idx;" \
        >/dev/null 2>>/run/hook.err || return 1
    psql_faultlab -c "CREATE INDEX CONCURRENTLY cic_k_idx ON cic(k);" \
        >/dev/null 2>>/run/hook.err || return 1
    reached "$BUILD_FINISHED"
    return 0
}

# pg_amcheck --heapallindexed is the official detector for this corruption: it
# verifies every live heap tuple has a matching index entry, which is exactly
# what a build that skipped HOT-pruned rows fails. Only that finding is the bug.
# pg_amcheck also exits non-zero when the server is down, when a connection
# drops mid-check, and when no valid index matches, none of which is evidence
# about the index, so those runs leave the oracle silent.
hook_amcheck() {
    if ! as_postgres "$PGBIN/pg_isready" -h /tmp -U postgres -d faultlab \
        >/dev/null 2>&1; then
        reached "$AMCHECK_SERVER_DOWN"
        return 0
    fi
    # The output file is private to this instance; several instances of the hook
    # can be in flight at once.
    out=/run/amcheck.$$.out
    as_postgres "$PGBIN/pg_amcheck" -h /tmp -U postgres -d faultlab \
        --heapallindexed --index=cic_k_idx >"$out" 2>&1
    status=$?
    if grep -q "lacks matching index tuple" "$out"; then
        reached "$AMCHECK_COMPARED"
        antithesis_assert always "$INDEX_COMPLETE" true false
    elif [ "$status" -eq 0 ]; then
        reached "$AMCHECK_COMPARED"
        antithesis_assert always "$INDEX_COMPLETE" true true
    else
        reached "$AMCHECK_FAILED"
    fi
    return 0
}

# With autovacuum off this is the one pruning event the search controls, so a
# state where it has run is a coverage goal in its own right.
hook_vacuum() {
    psql_faultlab -c "VACUUM cic;" >/dev/null 2>>/run/hook.err || return 1
    reached "$VACUUM_FINISHED"
    return 0
}

declare_assertions

case "$1" in
    1) hook_churn ;;
    2) hook_cic ;;
    3) hook_amcheck ;;
    4) hook_vacuum ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
