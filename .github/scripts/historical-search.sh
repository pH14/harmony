#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run one fresh search campaign for one arm of a historical-bug case.
#
# The vulnerable arm must find, inside the budget, a bug that carries the case's
# own oracle evidence and that replayed successfully from a fresh session; the
# control arm must find no such bug at all. historical-oracle.sh holds the rule.
# A miss on the vulnerable arm fails: this bug is expected to be found, so a
# miss says the machinery regressed rather than saying nothing.
set -euo pipefail

: "${CASE_ID:?}" "${ARM:?}" "${WORKLOAD_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${SEED:?}" "${WORKERS:?}" "${ACTIONS:?}" "${WALL_MINUTES:?}"
# An empty value is the locked no-knobs configuration. Keep it distinct from
# an omitted required variable so the case can prove it needs no tuning.
: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"
knobs=${KNOBS-}

case "${ARM}" in
    vulnerable) want=true ;;
    control) want=false ;;
    *) echo "unknown arm ${ARM}" >&2; exit 2 ;;
esac

oracle=$(dirname "$0")/historical-oracle.sh

harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/guest/bzImage-faultlab
base_initramfs=${PWD}/guest/initramfs.cpio.gz
chmod +x "${harmony}" "${agent}"

# A hosted runner's processor is drawn from a pool and decides how many
# executions the wall budget buys, so a campaign's execution count means
# nothing without it.
echo "runner cpu: $(grep -m1 '^model name' /proc/cpuinfo | cut -d: -f2- | sed 's/^ *//') x $(nproc)"

mkdir -p reports
out="reports/${CASE_ID}.${ARM}.search"
console="reports/${CASE_ID}.${ARM}.search.console.txt"
rm -rf "${out}"

# The wall budget is the campaign's only stopping rule, so no execution count
# is passed. The outer bound covers a run that stops answering instead.
status=0
timeout -k 60 "$(( (WALL_MINUTES + 20) * 60 ))" \
    "${harmony}" search --package faults \
    "oci-images/${IMAGE_PREFIX}-${WORKLOAD_VERSION}.oci" \
    --backend consonance \
    --kernel "${kernel}" \
    --base-initramfs "${base_initramfs}" \
    --fault-agent "${agent}" \
    --seed "${SEED}" \
    --workers "${WORKERS}" \
    --actions "${ACTIONS}" \
    --horizon-ms "${HORIZON_MS}" \
    --ram-mib "${RAM_MIB}" \
    --knobs "${knobs}" \
    --wall-minutes "${WALL_MINUTES}" \
    --out "${out}" >"${console}" 2>&1 || status=$?
tail -n 80 "${console}" || true

summary=${GITHUB_STEP_SUMMARY:-/dev/null}
report="${out}/report.json"
verdict=0

# A guest that stops answering ends its own execution and makes the CLI exit
# non-zero, while the campaign around it keeps running and still writes its
# report. The oracle reads that report and is the only thing that knows
# whether the arm behaved correctly, so a non-zero exit is recorded as a
# measure below and a missing report is the one failure decided here.
if [[ ! -s "${report}" ]]; then
    {
        echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
        echo
        echo "The campaign produced no report; the CLI exited ${status}."
    } >>"${summary}"
    exit 1
fi

# A guest the watchdog cut off leaves nothing in the report, so its count comes
# from the console the campaign wrote.
cutoff=$(grep -c 'without exiting' "${console}" || true)

outcome=$("${oracle}" search "${report}" "${ARM}") || verdict=1

{
    echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
    echo
    echo "| field | value |"
    echo "|---|---|"
    jq -r --arg want "${want}" --arg outcome "${outcome}" \
        --arg status "${status}" --arg cutoff "${cutoff}" \
        --arg assertion "${ORACLE_ASSERTION}" --arg evidence "${ORACLE_EVIDENCE}" '
        ["seed", (.seed | tostring)],
        ["workers", (.workers | tostring)],
        ["executions", (.executions | tostring)],
        ["horizons clocked", (.horizons_clocked | tostring)],
        ["wall seconds", (.wall_seconds | tostring)],
        ["bug found", (.bug_found | tostring)],
        ["bug expected", $want],
        ["confirmed bugs carrying the oracle assertion",
         ([.bugs[] | select(.confirmed and (.violations | index($assertion | tonumber)))] | length | tostring)],
        ["oracle verdict reached",
         ((.campaign_milestones.sometimes // 0) / pow(2; ($evidence | tonumber))
          | floor | . % 2 == 1 | tostring)],
        ["executions to first hit", (.first_bug_execution | tostring)],
        ["guests cut off by the watchdog", $cutoff],
        ["CLI exit status", $status],
        ["verdict", $outcome]
        | "| \(.[0]) | \(.[1]) |"
    ' "${report}"
    echo
    if [[ -s "${out}/campaign-summary.json" ]]; then
        echo "| measure | value |"
        echo "|---|---|"
        jq -r '.measures |
            ["guest seconds", (.guest_seconds | tostring)],
            ["acknowledged writes", (.acknowledged_writes | tostring)],
            ["kills fired", (.kills_fired | tostring)],
            ["kills unfired", (.kills_unfired | tostring)],
            ["median fired-kill age in ticks", (.kill_age_ticks_median | tostring)],
            ["maximum fired-kill age in ticks", (.kill_age_ticks_max | tostring)],
            ["conclusive checks", (.checks_conclusive | tostring)],
            ["inconclusive checks", (.checks_inconclusive | tostring)],
            ["fired site", (.fired_site // "none")]
            | "| \(.[0]) | \(.[1]) |"
        ' "${out}/campaign-summary.json"
    fi
} >>"${summary}"

exit "${verdict}"
