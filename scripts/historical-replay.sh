#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Replay one current-run input against one arm of a historical case.
#
#   historical-replay.sh discovery <first-bug-input.json>
#   historical-replay.sh sample <sample.json>
#
# The campaign itself already performs one fresh replay for every finding. A
# discovery replay therefore defaults to the fixed arm only; it provides the
# differential check without replaying the vulnerable finding a second time.
# Samples replay the vulnerable arm twice so the oracle can compare the two
# fresh state digests. Differential control coverage comes from discovery.
set -euo pipefail

mode=${1:?mode is required}
input=${2:?input is required}

: "${CASE_ID:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${VULNERABLE_VERSION:?}" "${CONTROL_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}" "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

case "${mode}" in
    discovery) default_arms=control; default_repeats=1 ;;
    sample) default_arms=vulnerable; default_repeats=2 ;;
    *) echo "historical-replay: unknown mode ${mode}" >&2; exit 2 ;;
esac

[[ -s "${input}" ]] || {
    echo "historical-replay: infra-failure (missing input ${input})" >&2
    exit 1
}

harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/guest/bzImage-faultlab
base_initramfs=${PWD}/guest/initramfs.cpio.gz
chmod +x "${harmony}" "${agent}"
test -x "${harmony}" && test -x "${agent}" && test -s "${kernel}" && test -s "${base_initramfs}"

oracle=$(dirname "$0")/historical-oracle.sh
knobs=${KNOBS:-}
arms=${REPLAY_ARMS:-${default_arms}}
repeats=${REPLAY_REPEATS:-${default_repeats}}
timeout_seconds=${REPLAY_TIMEOUT_SECONDS:-1800}
max_sessions=${MAX_REPLAY_SESSIONS:-2}

[[ "${repeats}" =~ ^[1-9][0-9]*$ ]] || {
    echo "historical-replay: infra-failure (repeat count must be positive)" >&2
    exit 1
}
[[ "${timeout_seconds}" =~ ^[1-9][0-9]*$ ]] || {
    echo "historical-replay: infra-failure (timeout must be positive)" >&2
    exit 1
}
[[ "${max_sessions}" =~ ^[1-9][0-9]*$ ]] || {
    echo "historical-replay: infra-failure (replay cap must be positive)" >&2
    exit 1
}

actions=$(jq -r 'if type == "array" then length else (.actions | length) end' "${input}")
[[ "${actions}" =~ ^[1-9][0-9]*$ ]] || {
    echo "historical-replay: infra-failure (input names no actions)" >&2
    exit 1
}

IFS=',' read -r -a arm_list <<<"${arms}"
session_count=$(( ${#arm_list[@]} * repeats ))
if (( session_count > max_sessions )); then
    echo "historical-replay: infra-failure (replay cap ${max_sessions} would be exceeded by ${session_count} sessions)" >&2
    exit 1
fi

mkdir -p reports
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
rows=()
verdict=0

replay_arm() {
    local arm=$1 version
    case "${arm}" in
        vulnerable) version=${VULNERABLE_VERSION} ;;
        control) version=${CONTROL_VERSION} ;;
        *) rows+=("| ${arm} | — | — | — | fail: infra-failure (unknown arm) |"); verdict=1; return ;;
    esac

    local out="reports/${CASE_ID}.${arm}.${mode}"
    local console="reports/${CASE_ID}.${arm}.${mode}.console.txt"
    rm -rf "${out}"
    local status=0
    timeout -k 30 "${timeout_seconds}" "${harmony}" search --package faults \
        "oci-images/${IMAGE_PREFIX}-${version}.oci" \
        --backend consonance \
        --kernel "${kernel}" \
        --base-initramfs "${base_initramfs}" \
        --fault-agent "${agent}" \
        --replay "${input}" \
        --repeat "${repeats}" \
        --horizon-ms "${HORIZON_MS}" \
        --ram-mib "${RAM_MIB}" \
        --knobs "${knobs}" \
        --out "${out}" >"${console}" 2>&1 || status=$?
    tail -n 40 "${console}" || true

    local report="${out}/report.json"
    if [[ ! -s "${report}" ]]; then
        rows+=("| ${arm} | ${version} | ${repeats} | — | fail: infra-failure (no report, exit ${status}) |")
        verdict=1
        return
    fi

    local ok=pass
    if (( status != 0 )); then
        ok="fail: infra-failure (CLI exit ${status})"
        verdict=1
    elif ! ok=$("${oracle}" "${mode}" "${report}" "${arm}" "${repeats}" "${actions}"); then
        verdict=1
    fi
    local hash_count
    hash_count=$(jq -r '[.replays[].state_hash] | unique | length' "${report}")
    jq -n \
        --arg mode "${mode}" --arg case_id "${CASE_ID}" --arg arm "${arm}" \
        --arg version "${version}" --arg input "${input}" \
        --arg status "${status}" --arg oracle "${ok}" \
        --argjson repeats "${repeats}" --argjson actions "${actions}" \
        --argjson state_hashes "${hash_count}" \
        '{mode:$mode, case_id:$case_id, arm:$arm, version:$version,
          input:$input, repeats:$repeats, actions:$actions,
          cli_exit_status:($status|tonumber), state_hash_count:$state_hashes,
          oracle:$oracle}' >"${out}/panel-status.json"
    rows+=("| ${arm} | ${version} | ${repeats} | ${hash_count} | ${ok} (cli exit ${status}) |")
}

for arm in "${arm_list[@]}"; do
    replay_arm "${arm}"
done

{
    echo "## ${mode} replay — ${input}"
    echo
    echo "| arm | ${SOFTWARE_NAME} | sessions | state-hash count | verdict |"
    echo "|---|---|---:|---:|---|"
    printf '%s\n' "${rows[@]}"
    echo
    echo "Replay cap: ${max_sessions} sessions; requested: ${session_count}."
} >>"${summary}"

exit "${verdict}"
