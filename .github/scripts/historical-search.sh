#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run one fresh search campaign for one arm of a historical-bug case.
#
# The vulnerable arm must find the bug inside the budget and the control arm
# must not find it at all. A miss on the vulnerable arm fails: this bug is
# expected to be found, so a miss says the machinery regressed rather than
# saying nothing.
set -euo pipefail

: "${CASE_ID:?}" "${ARM:?}" "${PG_VERSION:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${SEED:?}" "${WORKERS:?}" "${ACTIONS:?}" "${EXECUTIONS:?}" "${WALL_MINUTES:?}"
: "${KNOBS:?}"

case "${ARM}" in
    vulnerable) want=true ;;
    control) want=false ;;
    *) echo "unknown arm ${ARM}" >&2; exit 2 ;;
esac

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
    "oci-images/pgcic-${PG_VERSION}.oci" \
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
    --knobs "${KNOBS}" \
    --wall-minutes "${WALL_MINUTES}" \
    --out "${out}" >"${console}" 2>&1 || status=$?
tail -n 80 "${console}" || true

summary=${GITHUB_STEP_SUMMARY:-/dev/null}
report="${out}/report.json"
verdict=0

if [[ "${status}" -ne 0 ]] || [[ ! -s "${report}" ]]; then
    {
        echo "## Search campaign — ${ARM} (PostgreSQL ${PG_VERSION})"
        echo
        echo "The campaign produced no report; the CLI exited ${status}."
    } >>"${summary}"
    exit 1
fi

if jq -e --argjson want "${want}" '.mode == "search" and .bug_found == $want' \
    "${report}" >/dev/null; then
    outcome=pass
else
    outcome=fail
    verdict=1
fi

{
    echo "## Search campaign — ${ARM} (PostgreSQL ${PG_VERSION})"
    echo
    echo "| field | value |"
    echo "|---|---|"
    jq -r --arg want "${want}" --arg outcome "${outcome}" '
        ["seed", (.seed | tostring)],
        ["workers", (.workers | tostring)],
        ["executions", (.executions | tostring)],
        ["horizons clocked", (.horizons_clocked | tostring)],
        ["wall seconds", (.wall_seconds | tostring)],
        ["bug found", (.bug_found | tostring)],
        ["bug expected", $want],
        ["executions to first hit", (.first_bug_execution | tostring)],
        ["verdict", $outcome]
        | "| \(.[0]) | \(.[1]) |"
    ' "${report}"
} >>"${summary}"

exit "${verdict}"
