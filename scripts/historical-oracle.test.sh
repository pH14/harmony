#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Synthetic regression cases for the historical panel oracle.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
oracle=${here}/historical-oracle.sh
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
failures=0

expect() {
    local want=$1 name=$2 body=$3 mode=$4 repeats=${5:-} actions=${6:-}
    local report="${work}/report.json"
    printf '%s' "${body}" >"${report}"
    local got status=0
    got=$("${oracle}" "${mode}" "${report}" "${repeats}" "${actions}" 2>&1) || status=$?
    local verdict=fail
    [[ "${got}" == pass ]] && verdict=pass
    if [[ "${verdict}" != "${want}" ]]; then
        printf 'FAIL %-46s wanted %s, got %s (exit %s)\n' "${name}" "${want}" "${got}" "${status}"
        failures=$((failures + 1))
    else
        printf 'ok   %s (%s)\n' "${name}" "${got}"
    fi
}

replay_run() {
    jq -cn --argjson bug "$1" --argjson violations "$2" --argjson sometimes "$3" \
        --argjson applied "$4" --argjson horizons "$5" '{
        run: 1, bug: $bug, stop: "Assertion", state_hash: "abc",
        violations: $violations, sometimes: $sometimes,
        actions_applied: $applied, settle_actions: 0, settle_ticks: 0,
        guest_horizons: $horizons, check: null
    }'
}

replay_report() {
    jq -cn --argjson replays "[$1]" '{mode: "replay", replays: $replays}'
}

corrupt=$(replay_run true '["case-assertion"]' '["other","case-evidence"]' 7 7)
clean=$(replay_run false '[]' '["other","case-evidence"]' 7 7)
silent=$(replay_run false '[]' '["other"]' 7 7)
cached=$(replay_run false '[]' '["other","case-evidence"]' 5 2)

expect pass 'a clean no-find sample on the searched version' \
    "$(replay_report "${clean},${clean}")" sample 2 7
different_state=$(jq '.state_hash = "different"' <<<"${clean}")
expect fail 'sample repeats with different state digests' \
    "$(replay_report "${clean},${different_state}")" sample 2 7
expect fail 'a sample replay that violates the case assertion' \
    "$(replay_report "${corrupt}")" sample 1 7
expect fail 'a sample replay whose detector stayed silent' \
    "$(replay_report "${silent}")" sample 1 7
expect fail 'a replay answered by a cached prefix' \
    "$(replay_report "${cached}")" sample 1 7

checked=$(jq '.settle_actions = 3 | .settle_ticks = 7 | .guest_horizons += 3 |
    .check = {disturbance_generation:3, run:7,
    start_generation:3, end_generation:3, points:["case-evidence"], pending_faults:0}' <<<"${clean}")
expect pass 'continuous check completed after the final recovery' \
    "$(replay_report "${checked}")" sample 1 7
for mutation in \
    '.check.start_generation = 2 | .check.end_generation = 2' \
    '.check.start_generation = 2' \
    '.check.disturbance_generation = 4' \
    '.check.pending_faults = 1' \
    '.check.points = []' \
    '.check.run = 0'; do
    stale=$(jq "${mutation}" <<<"${checked}")
    expect fail "continuous check rejects ${mutation}" \
        "$(replay_report "${stale}")" sample 1 7
done
missing=$(jq 'del(.check)' <<<"${clean}")
expect fail 'missing checker provenance is not a legacy fallback' \
    "$(replay_report "${missing}")" sample 1 7

bug() {
    jq -cn --argjson confirmed "$1" --argjson violations "$2" --argjson sometimes "$3" \
        --argjson replay "${4:-null}" '{execution: 12, actions: [{"Wait":50}],
        stop: "Assertion", violations: $violations, sometimes: $sometimes,
        state_hash: "abc", confirmed: $confirmed, replay: $replay}'
}

search_report() {
    jq -cn --argjson found "$1" --argjson bugs "[$2]" \
        '{mode: "search", bug_found: $found, bugs: $bugs}'
}

confirmed_replay=$(replay_run true '["case-assertion"]' '["other","case-evidence"]' 1 1)
confirmed=$(bug true '["case-assertion"]' '["other","case-evidence"]' "${confirmed_replay}")
unconfirmed=$(bug false '["case-assertion"]' '["other","case-evidence"]')
wrong_replay=$(replay_run true '["another-assertion"]' '["other","case-evidence"]' 1 1)
wrong=$(bug true '["case-assertion"]' '["other","case-evidence"]' "${wrong_replay}")

expect pass 'a confirmed current-build discovery' \
    "$(search_report true "${confirmed}")" search
expect fail 'a search miss' \
    "$(search_report false '')" search
expect fail 'a found candidate without replay confirmation' \
    "$(search_report false "${unconfirmed}")" search
expect fail 'a confirmed candidate with replay mismatch' \
    "$(search_report true "${wrong}")" search
expect fail 'a fixed-version comparison mode is gone' \
    "$(replay_report "${clean}")" discovery 1 7

if (( failures > 0 )); then
    printf '%s check(s) failed\n' "${failures}"
    exit 1
fi
printf 'all historical-oracle checks passed\n'
