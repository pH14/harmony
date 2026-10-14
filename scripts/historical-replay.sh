#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Replay one declared clean sample, or a search's reproducer, against a
# historical case's affected version.
#
#   historical-replay.sh sample <sample.json>
#   historical-replay.sh reproduce <search-report.json>
#
# The campaign itself already performs one fresh replay for every finding.
# Samples replay twice, covering fresh starts and no-find paths. KVM compares
# state digests; UML checks observed evidence and marks state identity unavailable.
set -euo pipefail

mode=${1:?mode is required}
input=${2:?input is required}

: "${CASE_ID:?}" "${RAM_MIB:?}"
: "${WORKLOAD_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}" "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

case "${mode}" in
    sample) default_repeats=2 ;;
    reproduce) default_repeats=3 ;;
    *) echo "historical-replay: unknown mode ${mode}" >&2; exit 2 ;;
esac

[[ -s "${input}" ]] || {
    echo "historical-replay: infra-failure (missing input ${input})" >&2
    exit 1
}

harmony=${PWD}/tools/harmony
base_initramfs=${PWD}/guest/initramfs-oci.cpio.gz
chmod +x "${harmony}"
test -x "${harmony}" && test -s "${base_initramfs}"

oracle=$(dirname "$0")/historical-oracle.sh
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

finding=
if [[ "$mode" == reproduce ]]; then
    finding=$(jq -r --arg assertion "$ORACLE_ASSERTION" --arg evidence "$ORACLE_EVIDENCE" \
        -f "$(dirname "$0")/historical-confirmed-finding.jq" "$input")
    [[ "$finding" =~ ^[1-9][0-9]*$ ]] || {
        echo 'historical-replay: infra-failure (no confirmed case finding)' >&2
        exit 1
    }
    actions=$(jq -r --argjson finding "$finding" '.bugs[$finding - 1].actions | length' "$input")
else
    actions=$(jq -r 'if type == "array" then length else (.actions | length) end' "${input}")
fi
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
launcher=()
guest_arguments=()
# shellcheck source=historical-backend.sh disable=SC1091
. "$(dirname "$0")/historical-backend.sh"
historical_backend "reports/${CASE_ID}.${label}.denial.json"
if [[ "$mode" == reproduce ]]; then
    arguments=(debug replay "$(dirname "$input")" --finding "$finding" --repeat "$repeats")
else
    arguments=(debug run "oci-images/${IMAGE_PREFIX}-${WORKLOAD_VERSION}.oci"
        "${guest_arguments[@]}"
        --actions "$input" --repeat "$repeats")
fi
timeout -k 30 "$timeout_seconds" ${launcher[@]+"${launcher[@]}"} "$harmony" \
    "${arguments[@]}" --out "$out" >"$console" 2>&1 || status=$?
tail -n 40 "${console}" || true

report="${out}/report.json"
if [[ ! -s "${report}" ]]; then
    row="| ${WORKLOAD_VERSION} | ${repeats} | — | fail: infra-failure (no report, exit ${status}) |"
    verdict=1
    hash_count=0
else
    ok=pass
    identity_unavailable=false
    if [[ "${BACKEND:-consonance}" == uml && "$mode" == reproduce && "$status" -eq 2 ]] \
        && grep -Fq 'full UML state identity is unavailable' "$console" \
        && jq -e '(.replays | length) > 0 and all(.replays[];
            .state_hash_encoding == "unavailable" and .state_hash == "")' \
            "$report" >/dev/null 2>&1; then
        identity_unavailable=true
    fi
    if (( status > 1 )) && [[ "$identity_unavailable" != true ]]; then
        ok="fail: infra-failure (CLI exit ${status})"
        verdict=1
    elif ! ok=$("${oracle}" "${mode}" "${report}" "${repeats}" "${actions}"); then
        verdict=1
    fi
    hash_count_json=$(jq -c 'if (.replays | length) > 0
        and all(.replays[];
            .state_hash != null and .state_hash != ""
            and .state_hash_encoding != "unavailable")
        then ([.replays[].state_hash] | unique | length)
        else "unavailable" end' "$report")
    hash_count=$(jq -r . <<<"$hash_count_json")
    jq -n \
        --arg mode "${mode}" --arg case_id "${CASE_ID}" \
        --arg version "${WORKLOAD_VERSION}" --arg input "${input}" \
        --arg finding "${finding}" \
        --arg status "${status}" --arg oracle "${ok}" \
        --argjson repeats "${repeats}" --argjson actions "${actions}" \
        --argjson state_hashes "${hash_count_json}" \
        '{mode:$mode, case_id:$case_id, version:$version,
          input:$input, finding:(if $finding == "" then null else ($finding | tonumber) end),
          repeats:$repeats, actions:$actions,
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
