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
: "${SEED:?}" "${WORKERS:?}" "${ACTIONS:?}" "${EXECUTIONS:?}" "${WALL_MINUTES:?}"
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

mkdir -p reports
out="reports/${CASE_ID}.${ARM}.search"
console="reports/${CASE_ID}.${ARM}.search.console.txt"
rm -rf "${out}"

# The campaign stops itself at --wall-minutes; the outer bound covers a run
# that stops answering instead.
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
    --executions "${EXECUTIONS}" \
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

if [[ "${status}" -ne 0 ]] || [[ ! -s "${report}" ]]; then
    {
        echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
        echo
        echo "The campaign produced no report; the CLI exited ${status}."
    } >>"${summary}"
    exit 1
fi

outcome=$("${oracle}" search "${report}" "${ARM}") || verdict=1

{
    echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
    echo
    echo "| field | value |"
    echo "|---|---|"
    jq -r --arg want "${want}" --arg outcome "${outcome}" \
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
        ["verdict", $outcome]
        | "| \(.[0]) | \(.[1]) |"
    ' "${report}"
} >>"${summary}"

exit "${verdict}"
