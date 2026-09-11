#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks for historical-oracle.sh, run from the repository root.
#
# Each case is a synthetic report the workload could write, so the rules are
# exercised without a guest: only the expected oracle evidence passes, and a
# crash, another assertion, a silent detector or a cached prefix does not.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
oracle=${here}/historical-oracle.sh
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24

failures=0

# expect <verdict> <name> <report json> <oracle arguments...>
expect() {
    local want=$1 name=$2 body=$3
    shift 3
    local report="${work}/report.json"
    printf '%s' "${body}" >"${report}"
    local got status=0
    got=$("${oracle}" "$1" "${report}" "${2:-}" "${3:-}" "${4:-}" 2>&1) || status=$?
    local verdict=fail
    [[ "${got}" == pass ]] && verdict=pass
    if [[ "${verdict}" != "${want}" ]]; then
        printf 'FAIL %s: wanted %s, got %s (exit %s)\n' "${name}" "${want}" "${got}" "${status}"
        failures=$((failures + 1))
    else
        printf 'ok   %s (%s)\n' "${name}" "${got}"
    fi
}

# One replay run, spelled out so each case can change exactly one field.
run() {
    jq -cn --argjson bug "$1" --argjson stop "$2" --argjson violations "$3" \
        --argjson sometimes "$4" --argjson applied "$5" --argjson horizons "$6" '
        {
            run: 1, bug: $bug, stop: $stop, state_hash: "abc",
            violations: $violations, sometimes: $sometimes,
            actions_applied: $applied, guest_horizons: $horizons
        }'
}

replay_report() {
    jq -cn --argjson replays "[$1]" '{ mode: "replay", replays: $replays }'
}

corrupt=$(run true '{"Assertion":{"point":2}}' '[2]' '[22,24]' 5 5)
clean=$(run false '"Deadline"' '[]' '[22,24]' 7 7)
crash=$(run true '"Crash"' '[]' '[22]' 3 3)
other=$(run true '{"Assertion":{"point":9}}' '[9]' '[22,24]' 5 5)
silent=$(run false '"Deadline"' '[]' '[22,26]' 7 7)
crash_checked=$(run true '"Crash"' '[]' '[22,24]' 5 5)
cached=$(run true '{"Assertion":{"point":2}}' '[2]' '[22,24]' 5 2)

expect pass 'the expected corruption on the vulnerable arm' \
    "$(replay_report "${corrupt},${corrupt}")" replay vulnerable 2 7
expect fail 'a crash on the vulnerable arm' \
    "$(replay_report "${crash},${crash}")" replay vulnerable 2 7
expect fail 'a crash after the detector reached its verdict' \
    "$(replay_report "${crash_checked},${crash_checked}")" replay vulnerable 2 7
expect fail 'another assertion on the vulnerable arm' \
    "$(replay_report "${other},${other}")" replay vulnerable 2 7
expect fail 'one repeat of two reproducing nothing' \
    "$(replay_report "${corrupt},${clean}")" replay vulnerable 2 7
expect fail 'a run that reused a cached prefix' \
    "$(replay_report "${cached},${corrupt}")" replay vulnerable 2 7
expect fail 'fewer replay runs than the arm was asked for' \
    "$(replay_report "${corrupt}")" replay vulnerable 2 7

expect pass 'a clean control run that reached the verdict' \
    "$(replay_report "${clean}")" replay control 1 7
expect fail 'a control run whose detector never reached a verdict' \
    "$(replay_report "${silent}")" replay control 1 7
expect fail 'a control run that crashed' \
    "$(replay_report "${crash}")" replay control 1 7
expect fail 'a control run that crashed after the verdict' \
    "$(replay_report "${crash_checked}")" replay control 1 7
expect fail 'a control run that stopped short of its actions' \
    "$(replay_report "$(run false '"Deadline"' '[]' '[22,24]' 4 4)")" replay control 1 7

bug() {
    jq -cn --argjson confirmed "$1" --argjson violations "$2" --argjson sometimes "$3" \
        --argjson replay "${4:-null}" '
        { execution: 12, actions: [], stop: "Crash", violations: $violations,
          sometimes: $sometimes, state_hash: "abc", confirmed: $confirmed,
          replay: $replay }'
}

search_report() {
    local sometimes=${3:-0}
    jq -cn --argjson found "$1" --argjson bugs "[$2]" --argjson sometimes "${sometimes}" \
        '{ mode: "search", bug_found: $found, bugs: $bugs,
           campaign_milestones: { sometimes: $sometimes, hooks_finished: 0, bug: $found } }'
}

fresh_replay=$(run true '{"Assertion":{"point":2}}' '[2]' '[22,24]' 0 0)
confirmed=$(bug true '[2]' '[22,24]' "${fresh_replay}")
unconfirmed=$(bug false '[2]' '[22,24]')
unreplayed=$(bug true '[2]' '[22,24]')
crash_bug=$(bug true '[]' '[22]')

expect pass 'a confirmed corruption found by the campaign' \
    "$(search_report true "${confirmed}")" search vulnerable
expect fail 'a corruption no replay confirmed' \
    "$(search_report true "${unconfirmed}")" search vulnerable
expect fail 'a confirmed corruption without a fresh replay' \
    "$(search_report true "${unreplayed}")" search vulnerable
expect fail 'a crash standing in for the corruption' \
    "$(search_report true "${crash_bug}")" search vulnerable
expect pass 'a control campaign that found nothing' \
    "$(search_report false '' 16777216)" search control
expect fail 'a silent control campaign' \
    "$(search_report false '' 0)" search control
expect fail 'a control campaign that hit the assertion' \
    "$(search_report true "${confirmed}" 16777216)" search control
expect fail 'a control campaign with an unconfirmed bug' \
    "$(search_report false "${unconfirmed}" 16777216)" search control
expect fail 'a control campaign with another bug' \
    "$(search_report false "${crash_bug}" 16777216)" search control

# Exercise the real search wrapper's report rendering too. A malformed jq
# filter used to turn a successful, confirmed discovery into a failed CI step
# after the report had already been written.
search_root=${work}/search-root
search_summary=${work}/search-summary.md
mkdir -p "${search_root}/tools" "${search_root}/guest" "${search_root}/oci-images"
cat >"${search_root}/tools/harmony" <<'FAKE_HARMONY'
#!/usr/bin/env bash
set -euo pipefail
out=
while (($#)); do
    if [[ "$1" == --out ]]; then
        out=$2
        shift 2
    else
        shift
    fi
done
: "${out:?}" "${FAKE_SEARCH_REPORT:?}"
mkdir -p "${out}"
cp "${FAKE_SEARCH_REPORT}" "${out}/report.json"
echo "bug_found   true  executions 12  horizons 24"
if [[ -n "${FAKE_SEARCH_CUTOFF:-}" ]]; then
    echo "fault action Restart(1) failed: run: guest ran for more than 1024s of host time without exiting"
fi
exit "${FAKE_SEARCH_EXIT:-0}"
FAKE_HARMONY
: >"${search_root}/tools/fault-agent"
: >"${search_root}/guest/bzImage-faultlab"
: >"${search_root}/guest/initramfs.cpio.gz"
: >"${search_root}/oci-images/fake-1.0.oci"

# One wrapper run against that fake CLI, which exits with the given status and
# optionally prints the line a cut-off guest leaves in the console.
run_search_wrapper() {
    : >"${search_summary}"
    (
        cd "${search_root}"
        export CASE_ID=fake ARM=vulnerable SOFTWARE_NAME=fake WORKLOAD_VERSION=1.0
        export IMAGE_PREFIX=fake HORIZON_MS=500 RAM_MIB=128 SEED=1 WORKERS=1
        export ACTIONS=1 EXECUTIONS=12 WALL_MINUTES=1 KNOBS=
        export FAKE_SEARCH_REPORT="$1" FAKE_SEARCH_EXIT="$2" FAKE_SEARCH_CUTOFF="${3:-}"
        export GITHUB_STEP_SUMMARY="${search_summary}"
        "${here}/historical-search.sh"
    )
}

search_fixture=${work}/search-fixture.json
jq -cn --argjson bug "${confirmed}" '
    {
        mode: "search", seed: 1, workers: 1, executions: 12,
        horizons_clocked: 24, wall_seconds: 1, bug_found: true,
        first_bug_execution: 12, bugs: [$bug],
        campaign_milestones: { sometimes: 16777216, hooks_finished: 1, bug: true }
    }' >"${search_fixture}"
if ! run_search_wrapper "${search_fixture}" 0; then
    printf 'FAIL search wrapper rejected a confirmed discovery\n'
    failures=$((failures + 1))
elif ! grep -qF '| oracle verdict reached | true |' "${search_summary}" \
    || ! grep -qF '| confirmed bugs carrying the oracle assertion | 1 |' "${search_summary}" \
    || ! grep -qF '| verdict | pass |' "${search_summary}"; then
    printf 'FAIL search wrapper omitted the passing verdict\n'
    failures=$((failures + 1))
else
    printf 'ok   search wrapper renders a confirmed discovery\n'
fi

# A guest cut off by the watchdog ends its own execution and makes the CLI exit
# non-zero, while the campaign around it runs on and still writes its report.
# The report decides, and the cut-off guest is reported as a measure.
if ! run_search_wrapper "${search_fixture}" 1 cutoff; then
    printf 'FAIL search wrapper failed a confirmed discovery over a non-zero exit\n'
    failures=$((failures + 1))
elif ! grep -qF '| verdict | pass |' "${search_summary}" \
    || ! grep -qF '| guests cut off by the watchdog | 1 |' "${search_summary}" \
    || ! grep -qF '| CLI exit status | 1 |' "${search_summary}"; then
    printf 'FAIL search wrapper omitted the cut-off guest or the exit status\n'
    failures=$((failures + 1))
else
    printf 'ok   search wrapper judges a campaign whose guest was cut off\n'
fi

# A run that wrote no report at all has nothing for the oracle to judge.
if run_search_wrapper /dev/null 1 2>/dev/null; then
    printf 'FAIL search wrapper passed a campaign that wrote no report\n'
    failures=$((failures + 1))
else
    printf 'ok   search wrapper fails a campaign that wrote no report\n'
fi

if ((failures > 0)); then
    printf '%s check(s) failed\n' "${failures}"
    exit 1
fi
printf 'all historical-oracle checks passed\n'
