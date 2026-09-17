#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Replay one declared clean sample against a historical case's affected version.
#
#   historical-replay.sh sample <sample.json>
#
# The campaign itself already performs one fresh replay for every finding.
# Samples replay twice so the oracle can compare the two fresh state digests,
# which covers fresh starts and no-find paths.
set -euo pipefail

mode=${1:?mode is required}
input=${2:?input is required}

: "${CASE_ID:?}" "${RAM_MIB:?}"
: "${WORKLOAD_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}" "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

case "${mode}" in
    sample) default_repeats=2 ;;
    *) echo "historical-replay: unknown mode ${mode}" >&2; exit 2 ;;
esac

[[ -s "${input}" ]] || {
    echo "historical-replay: infra-failure (missing input ${input})" >&2
    exit 1
}

harmony=${PWD}/tools/harmony
kernel=${PWD}/guest/bzImage
base_initramfs=${PWD}/guest/initramfs-oci.cpio.gz
chmod +x "${harmony}"
test -x "${harmony}" && test -s "${kernel}" && test -s "${base_initramfs}"

oracle=$(dirname "$0")/historical-oracle.sh
knobs=${KNOBS:-}
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

if (( repeats > max_sessions )); then
    echo "historical-replay: infra-failure (replay cap ${max_sessions} would be exceeded by ${repeats} sessions)" >&2
    exit 1
fi

mkdir -p reports
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
verdict=0

label="${mode}-$(basename "${input}" .json)"
out="reports/${CASE_ID}.${label}"
console="reports/${CASE_ID}.${label}.console.txt"
rm -rf "${out}"
status=0
timeout -k 30 "${timeout_seconds}" "${harmony}" search --package faults \
    "oci-images/${IMAGE_PREFIX}-${WORKLOAD_VERSION}.oci" \
    --backend consonance \
    --kernel "${kernel}" \
    --base-initramfs "${base_initramfs}" \
    --replay "${input}" \
    --repeat "${repeats}" \
    --ram-mib "${RAM_MIB}" \
    --knobs "${knobs}" \
    --out "${out}" >"${console}" 2>&1 || status=$?
tail -n 40 "${console}" || true

report="${out}/report.json"
if [[ ! -s "${report}" ]]; then
    row="| ${WORKLOAD_VERSION} | ${repeats} | — | fail: infra-failure (no report, exit ${status}) |"
    verdict=1
    hash_count=0
else
    ok=pass
    if (( status != 0 )); then
        ok="fail: infra-failure (CLI exit ${status})"
        verdict=1
    elif ! ok=$("${oracle}" "${mode}" "${report}" "${repeats}" "${actions}"); then
        verdict=1
    fi
    hash_count=$(jq -r '[.replays[].state_hash] | unique | length' "${report}")
    jq -n \
        --arg mode "${mode}" --arg case_id "${CASE_ID}" \
        --arg version "${WORKLOAD_VERSION}" --arg input "${input}" \
        --arg status "${status}" --arg oracle "${ok}" \
        --argjson repeats "${repeats}" --argjson actions "${actions}" \
        --argjson state_hashes "${hash_count}" \
        '{mode:$mode, case_id:$case_id, version:$version,
          input:$input, repeats:$repeats, actions:$actions,
          cli_exit_status:($status|tonumber), state_hash_count:$state_hashes,
          oracle:$oracle}' >"${out}/panel-status.json"
    row="| ${WORKLOAD_VERSION} | ${repeats} | ${hash_count} | ${ok} (cli exit ${status}) |"
fi

{
    echo "## ${mode} replay — ${input}"
    echo
    echo "| ${SOFTWARE_NAME} | sessions | state-hash count | verdict |"
    echo "|---|---:|---:|---|"
    echo "${row}"
    echo
    echo "Replay cap: ${max_sessions} sessions; requested: ${repeats}."
} >>"${summary}"

exit "${verdict}"
