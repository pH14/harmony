#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exercises the sqlite-general oracle without Harmony: a clean run with
# client kills must evaluate its properties without a violation, resolved
# indeterminate commits and absent failed commits must pass, and a mismatched
# indeterminate commit, an applied failed commit, a lost acknowledged write and
# corrupted page headers must each be reported.
#
#   oracle-test.sh SQLITE_GENERAL SQLITE3_SHELL
set -euo pipefail

driver=$1
shell=$2
work=$(mktemp -d)
trap 'kill -9 $(jobs -p) 2>/dev/null || true; rm -rf "$work"' EXIT
export ANTITHESIS_OUTPUT_DIR=$work/out
mkdir -p "$ANTITHESIS_OUTPUT_DIR"
db=$work/test.db

hits() { grep -h '"hit":true' "$ANTITHESIS_OUTPUT_DIR/sdk.jsonl" | grep -F "\"id\":\"$1\"" | grep -c "\"condition\":$2" || true; }

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

run_clients() {
    local seconds=$1
    shift
    local -A pids=()
    for id in "$@"; do "$driver" client "$id" "$db" 2>/dev/null & pids[$id]=$!; done
    local end=$((SECONDS + seconds))
    while (( SECONDS < end )); do
        sleep 0.7
        if [[ "${KILLS:-0}" == 1 ]]; then
            local id=${*: RANDOM % $# + 1:1}
            kill -9 "${pids[$id]}" 2>/dev/null || true
            wait "${pids[$id]}" 2>/dev/null || true
            "$driver" client "$id" "$db" 2>/dev/null & pids[$id]=$!
        fi
    done
    for id in "$@"; do kill -9 "${pids[$id]}" 2>/dev/null || true; done
    wait 2>/dev/null || true
}

"$driver" init "$db"
KILLS=1 run_clients 12 0 1 2
for id in "sqlite integrity_check returns ok" "sqlite reports no corruption" \
    "sqlite preserves acknowledged commits" "sqlite read transaction sees one snapshot"; do
    (( $(hits "$id" false) == 0 )) || fail "clean run violated: $id"
done
(( $(hits "sqlite integrity_check completed" true) > 0 )) || fail "no integrity evidence"
(( $(hits "sqlite general compared committed rows" true) > 0 )) || fail "no comparison evidence"
echo "clean run: no violation, $(hits "sqlite integrity_check completed" true) integrity checks"

for id in 0 1 2; do "$driver" verify "$id" "$db" 2>/dev/null; done
(( $(hits "sqlite preserves acknowledged commits" false) == 0 )) || fail "the recovery comparison after the clean run violated"
for id in 0 1 2; do ! grep -q "^P" <(tail -n 1 "$work/journal-$id") || fail "client $id left a pending commit"; done

empty_hash=14695981039346656037
key=$(( (1 << 40) * 2 + 900000 ))
before=$(hits "sqlite preserves acknowledged commits" false)
printf 'P 900001 1 I:%d:1:0:%s\n' "$key" "$empty_hash" >>"$work/journal-1"
"$driver" verify 1 "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) == before )) || fail "an unapplied indeterminate commit was a violation"
grep -q "^R 900001 0$" "$work/journal-1" || fail "the unapplied commit was not resolved"

printf 'P 900002 1 I:%d:1:0:%s\n' "$((key + 1))" "$empty_hash" >>"$work/journal-1"
"$shell" "$db" "INSERT INTO kv(k, owner, ver, body) VALUES($((key + 1)), 1, 1, x'')"
"$driver" verify 1 "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) == before )) || fail "an applied indeterminate commit was a violation"
grep -q "^R 900002 1$" "$work/journal-1" || fail "the applied commit was not resolved"

printf 'P 900003 1 I:%d:1:0:%s\n' "$((key + 2))" "$empty_hash" >>"$work/journal-1"
"$shell" "$db" "INSERT INTO kv(k, owner, ver, body) VALUES($((key + 2)), 1, 7, x'')"
"$driver" verify 1 "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) > before )) || fail "an indeterminate commit matching neither outcome passed"
echo "indeterminate commits: unapplied and applied resolve, a mismatch is reported"

"$shell" "$db" "DELETE FROM kv WHERE k = $((key + 2))"
"$driver" verify 1 "$db" 2>/dev/null
before=$(hits "sqlite preserves acknowledged commits" false)
printf 'P 900004 1 I:%d:1:0:%s\nF 900004\n' "$((key + 3))" "$empty_hash" >>"$work/journal-1"
"$driver" verify 1 "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) == before )) || fail "a failed commit that did not apply was a violation"
printf 'P 900005 1 I:%d:1:0:%s\nF 900005\n' "$((key + 4))" "$empty_hash" >>"$work/journal-1"
"$shell" "$db" "INSERT INTO kv(k, owner, ver, body) VALUES($((key + 4)), 1, 1, x'')"
"$driver" verify 1 "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) > before )) || fail "a failed commit that applied passed"
echo "failed commits: absent passes, applied is reported"

before=$(hits "sqlite preserves acknowledged commits" false)
owner=$("$shell" "$db" "SELECT owner FROM kv WHERE owner IN (0, 2) ORDER BY owner LIMIT 1")
[[ -n "$owner" ]] || fail "no client holds a committed row"
victim=$("$shell" "$db" "SELECT max(k) FROM kv WHERE owner = $owner")
"$shell" "$db" "DELETE FROM kv WHERE k = $victim"
"$driver" verify "$owner" "$db" 2>/dev/null
(( $(hits "sqlite preserves acknowledged commits" false) > before )) || fail "a lost acknowledged row was not reported"
echo "lost acknowledged write: reported"

"$shell" "$db" "PRAGMA wal_checkpoint(TRUNCATE)" >/dev/null
pages=$(( $(stat -c %s "$db") / 4096 ))
(( pages > 3 )) || fail "database too small to corrupt"
for page in $(seq 1 $((pages - 1))); do
    printf '\xff%.0s' $(seq 1 16) | dd of="$db" bs=1 seek=$((page * 4096)) conv=notrunc status=none
done
before_integrity=$(hits "sqlite integrity_check returns ok" false)
before_corrupt=$(hits "sqlite reports no corruption" false)
"$driver" verify 2 "$db" 2>/dev/null || true
(( $(hits "sqlite integrity_check returns ok" false) > before_integrity ||
   $(hits "sqlite reports no corruption" false) > before_corrupt )) || fail "corrupted page headers were not reported"
echo "corrupted page headers: reported"
echo "PASS: sqlite-general oracle"
