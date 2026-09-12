#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run one fresh, bounded historical search campaign for one manifest arm.
#
# The input image and tools are built by the current checkout. The campaign is
# bounded by both its deterministic execution budget and its wall budget. Its
# report carries the first current-build reproducer; historical-search does not
# replay a committed witness or make a prior witness a gate.
set -euo pipefail

: "${CASE_ID:?}" "${ARM:?}" "${WORKLOAD_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${SEED:?}" "${WORKERS:?}" "${ACTIONS:?}" "${EXECUTIONS:?}" "${WALL_MINUTES:?}"
: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"
knobs=${KNOBS-}

for name in HORIZON_MS RAM_MIB SEED WORKERS ACTIONS EXECUTIONS WALL_MINUTES; do
    value=${!name}
    [[ "${value}" =~ ^[1-9][0-9]*$ ]] || {
        echo "historical-search: infra-failure (${name} must be positive)" >&2
        exit 1
    }
done

case "${ARM}" in
    vulnerable|control) ;;
    *) echo "historical-search: infra-failure (unknown arm ${ARM})" >&2; exit 2 ;;
esac

oracle=$(dirname "$0")/historical-oracle.sh
harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/guest/bzImage-faultlab
base_initramfs=${PWD}/guest/initramfs.cpio.gz
chmod +x "${harmony}" "${agent}"
test -x "${harmony}" && test -x "${agent}" && test -s "${kernel}" && test -s "${base_initramfs}"

mkdir -p reports
out="reports/${CASE_ID}.${ARM}.search"
console="reports/${CASE_ID}.${ARM}.search.console.txt"
rm -rf "${out}"

# The outer bound covers a process that stops answering after the campaign's
# own wall budget. A guest watchdog cutoff is a structured workload result and
# is preserved as measured status; an outer CLI timeout is infrastructure
# failure even when it left a partial report behind.
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
if [[ ! -s "${report}" ]]; then
    {
        echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
        echo
        echo "| field | value |"
        echo "|---|---|"
        echo "| execution status | infra-failure |"
        echo "| CLI exit status | ${status} |"
        echo "| report | missing |"
    } >>"${summary}"
    exit 1
fi

watchdog_cutoffs=0
for structured in "${report}" "${out}/campaign-summary.json"; do
    [[ -s "${structured}" ]] || continue
    candidate=$(jq -r '
        [
          .watchdog_cutoffs,
          .guest_watchdog_cutoffs,
          .campaign_watchdog_cutoffs,
          .fault_watchdog_cutoffs,
          .archive.watchdog_cutoffs
        ]
        | map(select(type == "number" and . >= 0 and floor == .))
        | if length == 0 then 0 else .[0] end
    ' "${structured}") || {
        echo "historical-search: infra-failure (invalid watchdog count in ${structured})" >&2
        exit 1
    }
    [[ "${candidate}" =~ ^[0-9]+$ ]] || {
        echo "historical-search: infra-failure (watchdog count is not an integer)" >&2
        exit 1
    }
    if (( watchdog_cutoffs != 0 && candidate != 0 && candidate != watchdog_cutoffs )); then
        echo "historical-search: infra-failure (report and summary disagree on watchdog cutoffs)" >&2
        exit 1
    fi
    if (( candidate > watchdog_cutoffs )); then
        watchdog_cutoffs=${candidate}
    fi
done

outcome=pass
if (( status != 0 )); then
    outcome="fail: infra-failure (CLI exit ${status})"
    verdict=1
elif ! outcome=$("${oracle}" search "${report}" "${ARM}"); then
    verdict=1
else
    verdict=0
fi

execution_status=complete
if (( status != 0 )); then
    execution_status=infra_failure
elif (( watchdog_cutoffs > 0 )); then
    execution_status=completed_with_watchdog_cutoffs
fi

# Keep the manifest, configuration and first current-build reproducer beside
# the report. These are diagnostics and provenance; the oracle still judges
# the structured report.
if [[ -n "${CASE_DIR:-}" && -s "${CASE_DIR}/case.json" ]]; then
    cp "${CASE_DIR}/case.json" "${out}/case.json"
fi
reproducer=
if [[ -s "${out}/first-bug-input.json" ]]; then
    reproducer="${out}/first-bug-input.json"
fi
jq -n \
    --arg case_id "${CASE_ID}" --arg arm "${ARM}" --arg software "${SOFTWARE_NAME}" \
    --arg version "${WORKLOAD_VERSION}" --arg image_prefix "${IMAGE_PREFIX}" \
    --arg oracle "${outcome}" --arg execution_status "${execution_status}" \
    --argjson cli_exit_status "${status}" --argjson watchdog_cutoffs "${watchdog_cutoffs}" \
    --argjson seed "${SEED}" --argjson workers "${WORKERS}" \
    --argjson executions_budget "${EXECUTIONS}" --argjson actions "${ACTIONS}" \
    --argjson horizon_ms "${HORIZON_MS}" --argjson ram_mib "${RAM_MIB}" \
    --argjson wall_minutes "${WALL_MINUTES}" --arg knobs "${knobs}" \
    --arg reproducer "${reproducer}" \
    '{case_id:$case_id, arm:$arm, software:$software, version:$version,
      image_prefix:$image_prefix, seed:$seed, workers:$workers,
      executions_budget:$executions_budget, actions:$actions,
      horizon_ms:$horizon_ms, ram_mib:$ram_mib, wall_minutes:$wall_minutes,
      knobs:$knobs, execution_status:$execution_status,
      cli_exit_status:$cli_exit_status, watchdog_cutoffs:$watchdog_cutoffs,
      oracle:$oracle,
      reproducer:(if $reproducer == "" then null else $reproducer end)}' \
    /dev/null >"${out}/panel-status.json"

{
    echo "## Search campaign — ${ARM} (${SOFTWARE_NAME} ${WORKLOAD_VERSION})"
    echo
    echo "| field | value |"
    echo "|---|---|"
    jq -r --arg outcome "${outcome}" --arg execution_status "${execution_status}" \
        --arg status "${status}" --arg cutoff "${watchdog_cutoffs}" --arg assertion "${ORACLE_ASSERTION}" \
        --arg evidence "${ORACLE_EVIDENCE}" '
        ["seed", (.seed | tostring)],
        ["workers", (.workers | tostring)],
        ["executions", (.executions | tostring)],
        ["horizons clocked", (.horizons_clocked | tostring)],
        ["wall seconds", (.wall_seconds | tostring)],
        ["bug found", (.bug_found | tostring)],
        ["raw findings", ((.bugs // []) | length | tostring)],
        ["confirmed findings carrying assertion", (([.bugs[]? | select(.confirmed and ((.violations // []) | index($assertion | tonumber)) != null)] | length) | tostring)],
        ["oracle verdict evidence", (([.bugs[]?.sometimes[]? | select(. == ($evidence | tonumber))] | length > 0) | tostring)],
        ["executions to first hit", (.first_bug_execution | tostring)],
        ["execution status", $execution_status],
        ["guests cut off by watchdog", $cutoff],
        ["CLI exit status", $status],
        ["verdict", $outcome]
        | "| \(.[0]) | \(.[1]) |"' "${report}"
    echo
    if [[ -s "${out}/campaign-summary.json" ]]; then
        echo "| measure | value |"
        echo "|---|---|"
        jq -r '.measures // {} |
            ["guest seconds", (.guest_seconds // "—" | tostring)],
            ["acknowledged writes", (.acknowledged_writes // "—" | tostring)],
            ["kills fired", (.kills_fired // "—" | tostring)],
            ["kills unfired", (.kills_unfired // "—" | tostring)],
            ["conclusive checks", (.checks_conclusive // "—" | tostring)],
            ["inconclusive checks", (.checks_inconclusive // "—" | tostring)]
            | "| \(.[0]) | \(.[1]) |"' "${out}/campaign-summary.json"
    fi
} >>"${summary}"

exit "${verdict}"
