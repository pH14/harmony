#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exercises the pg-general oracle without Harmony against a copy of the seeded
# cluster: a clean run across an immediate server restart must evaluate its
# properties without a violation, and a lost acknowledged write and a heap
# tuple whose indexed value no longer matches its index entry must each be
# reported. Runs as the cluster owner.
#
#   oracle-test.sh PG_GENERAL DATA_DIR
set -euo pipefail

driver=$1
work=$(mktemp -d)
data=$work/data
cp -a "$2" "$data"
bin=/usr/lib/postgresql/bin
export ANTITHESIS_OUTPUT_DIR=$work/out
mkdir -p "$ANTITHESIS_OUTPUT_DIR"
psql() { "$bin/psql" -h /tmp -U postgres -d faultlab -v ON_ERROR_STOP=1 -qtAX "$@"; }
server() { "$bin/pg_ctl" -D "$data" -w -t 120 -o "-k /tmp -c listen_addresses=''" -l "$work/server.log" "$@" >/dev/null; }
trap 'kill -9 -- "-${driver_pid:-0}" $(jobs -p) 2>/dev/null || true; server stop -m immediate 2>/dev/null || true; rm -rf "$work"' EXIT

hits() { grep -h '"hit":true' "$ANTITHESIS_OUTPUT_DIR/sdk.jsonl" | grep -F "\"id\":\"$1\"" | grep -c "\"condition\":$2" || true; }
fail() {
    echo "FAIL: $*" >&2
    tail -n 20 "$work/server.log" >&2 || true
    exit 1
}

server start
setsid "$driver" 2>"$work/driver.err" &
driver_pid=$!
sleep 10
server restart -m immediate
sleep 15
for id in "postgres amcheck finds every heap tuple indexed" "postgres index and sequential scans agree" \
    "postgres preserves acknowledged commits"; do
    (( $(hits "$id" false) == 0 )) || fail "clean run violated: $id"
done
(( $(hits "postgres amcheck verified an index" true) > 0 )) || fail "no amcheck evidence"
(( $(hits "postgres general compared committed rows" true) > 0 )) || fail "no comparison evidence"
echo "clean run with a restart: no violation, $(hits "postgres amcheck verified an index" true) index checks"

psql -c "DELETE FROM items WHERE id = (SELECT max(id) FROM items WHERE owner = 0)"
sleep 10
kill -9 -- "-$driver_pid" 2>/dev/null || true
wait 2>/dev/null || true
sleep 1
(( $(hits "postgres preserves acknowledged commits" false) > 0 )) || fail "a lost acknowledged row was not reported"
echo "lost acknowledged write: reported"

psql -c "DROP INDEX IF EXISTS items_a_idx" \
    -c "UPDATE items SET a = 1515870810 WHERE id = (SELECT min(id) FROM items)" \
    -c "CREATE INDEX items_a_idx ON items(a)" -c "CHECKPOINT"
heap=$data/$(psql -c "SELECT pg_relation_filepath('items')")
server stop -m fast
offset=$(grep -obUaP '\x5a\x5a\x5a\x5a' "$heap" | head -n 1 | cut -d: -f1)
[[ -n "$offset" ]] || fail "the marked heap value is not in the heap file"
printf '\x5b\x5b\x5b\x5b' | dd of="$heap" bs=1 seek="$offset" conv=notrunc status=none
server start
before=$(hits "postgres amcheck finds every heap tuple indexed" false)
"$driver" amcheck 2>/dev/null || true
(( $(hits "postgres amcheck finds every heap tuple indexed" false) > before )) || fail "a heap tuple without an index entry was not reported"
echo "heap tuple without an index entry: reported"
echo "PASS: pg-general oracle"
