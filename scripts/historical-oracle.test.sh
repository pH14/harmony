#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Synthetic regression cases for the historical panel oracle.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
oracle=${here}/historical-oracle.sh
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24
failures=0

expect() {
    local want=$1 name=$2 body=$3 mode=$4 arm=$5 repeats=${6:-} actions=${7:-}
    local report="${work}/report.json"
    printf '%s' "${body}" >"${report}"
    local got status=0
    got=$("${oracle}" "${mode}" "${report}" "${arm}" "${repeats}" "${actions}" 2>&1) || status=$?
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
        actions_applied: $applied, guest_horizons: $horizons
    }'
}

replay_report() {
    jq -cn --argjson replays "[$1]" '{mode: "replay", replays: $replays}'
}

corrupt=$(replay_run true '[2]' '[22,24]' 5 5)
corrupt_control=$(replay_run true '[2]' '[22,24]' 7 7)
clean=$(replay_run false '[]' '[22,24]' 7 7)
other=$(replay_run true '[9]' '[22,24]' 5 5)
silent=$(replay_run false '[]' '[22]' 7 7)
cached=$(replay_run false '[]' '[22,24]' 5 2)

expect pass 'a vulnerable finding with matching detector evidence' \
    "$(replay_report "${corrupt}")" replay vulnerable 1 7
expect pass 'a control differential replay that reaches its detector' \
    "$(replay_report "${clean}")" discovery control 1 7
expect pass 'a clean no-find sample on the vulnerable arm' \
    "$(replay_report "${clean},${clean}")" sample vulnerable 2 7
different_state=$(jq '.state_hash = "different"' <<<"${clean}")
expect fail 'sample repeats with different state digests' \
    "$(replay_report "${clean},${different_state}")" sample vulnerable 2 7
expect fail 'a different vulnerable assertion' \
    "$(replay_report "${other}")" replay vulnerable 1 7
expect fail 'a control replay that violates an assertion' \
    "$(replay_report "${corrupt_control}")" discovery control 1 7
expect fail 'a control replay whose detector stayed silent' \
    "$(replay_report "${silent}")" discovery control 1 7
expect fail 'a replay answered by a cached prefix' \
    "$(replay_report "${cached}")" sample control 1 7

bug() {
    jq -cn --argjson confirmed "$1" --argjson violations "$2" --argjson sometimes "$3" \
        --argjson replay "${4:-null}" '{execution: 12, actions: ["Wait"],
        stop: "Assertion", violations: $violations, sometimes: $sometimes,
        state_hash: "abc", confirmed: $confirmed, replay: $replay}'
}

search_report() {
    jq -cn --argjson found "$1" --argjson bugs "[$2]" \
        '{mode: "search", bug_found: $found, bugs: $bugs}'
}

confirmed_replay=$(replay_run true '[2]' '[22,24]' 1 1)
confirmed=$(bug true '[2]' '[22,24]' "${confirmed_replay}")
unconfirmed=$(bug false '[2]' '[22,24]')
wrong_replay=$(replay_run true '[9]' '[22,24]' 1 1)
wrong=$(bug true '[2]' '[22,24]' "${wrong_replay}")

expect pass 'a confirmed current-build discovery' \
    "$(search_report true "${confirmed}")" search vulnerable
expect fail 'a search miss' \
    "$(search_report false '')" search vulnerable
expect fail 'a found candidate without replay confirmation' \
    "$(search_report false "${unconfirmed}")" search vulnerable
expect fail 'a confirmed candidate with replay mismatch' \
    "$(search_report true "${wrong}")" search vulnerable
expect pass 'a clean control search' \
    "$(search_report false '')" search control
expect fail 'a control campaign violation' \
    "$(search_report true "${confirmed}")" search control

if (( failures > 0 )); then
    printf '%s check(s) failed\n' "${failures}"
    exit 1
fi
printf 'all historical-oracle checks passed\n'
