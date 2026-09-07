#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Replay one committed action list on both arms of a historical-bug case.
#
#   historical-replay.sh <mode> <input.json>
#
# The vulnerable arm must report the bug on every repeat and the control arm on
# none. Anything else fails, including a replay that produced no report at all.
# The step summary is written whether the rule holds or not, because a failing
# run is the evidence a reader most wants to see.
set -euo pipefail

mode=$1
input=$2

: "${CASE_ID:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${VULNERABLE_VERSION:?}" "${CONTROL_VERSION:?}"

# Two repeats on the arm that is supposed to fire, so a single lucky run cannot
# carry the claim; one on the control, which only has to stay silent.
vulnerable_repeats=${VULNERABLE_REPEATS:-2}
control_repeats=${CONTROL_REPEATS:-1}

harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/kernel/bzImage-faultlab
chmod +x "${harmony}" "${agent}"
test -f "${input}"

mkdir -p reports
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
rows=()
verdict=0

replay_arm() {
    local arm=$1 version=$2 repeats=$3 want=$4
    local out="reports/${CASE_ID}.${arm}.${mode}"
    local console="reports/${CASE_ID}.${arm}.${mode}.console.txt"
    rm -rf "${out}"

    # A guest that never reaches its deadline would otherwise eat the whole job
    # timeout and leave the other arm unrun.
    local status=0
    timeout -k 30 1800 "${harmony}" search --package faults \
        "oci-images/pgcic-${version}.oci" \
        --kernel "${kernel}" \
        --fault-agent "${agent}" \
        --replay "${input}" \
        --repeat "${repeats}" \
        --horizon-ms "${HORIZON_MS}" \
        --ram-mib "${RAM_MIB}" \
        --out "${out}" >"${console}" 2>&1 || status=$?
    tail -n 40 "${console}" || true

    local report="${out}/report.json"
    if [[ "${status}" -ne 0 ]] || [[ ! -s "${report}" ]]; then
        rows+=("| ${arm} | ${version} | ${repeats} | — | ${want} | — | no report (exit ${status}) |")
        verdict=1
        return
    fi

    local bugs hashes_agree
    bugs=$(jq -r '[.replays[] | select(.bug)] | length' "${report}")
    hashes_agree=$(jq -r '[.replays[].state_hash] | unique | length == 1' "${report}")

    # The oracle rule. State-hash agreement is reported beside it and is not
    # part of the rule: hosted runners have stock KVM, where the guest can read
    # the host counter directly.
    local ok=fail
    if jq -e --argjson n "${repeats}" --argjson want "${want}" '
        .mode == "replay"
        and (.replays | length) == $n
        and all(.replays[]; .bug == $want)
    ' "${report}" >/dev/null; then
        ok=pass
    else
        verdict=1
    fi
    rows+=("| ${arm} | ${version} | ${repeats} | ${bugs} | ${want} | ${hashes_agree} | ${ok} |")
}

replay_arm vulnerable "${VULNERABLE_VERSION}" "${vulnerable_repeats}" true
replay_arm control "${CONTROL_VERSION}" "${control_repeats}" false

{
    echo "## ${mode} replay — ${input}"
    echo
    echo "| arm | PostgreSQL | repeats | bugs reported | bug expected | state hashes agree | verdict |"
    echo "|---|---|---|---|---|---|---|"
    printf '%s\n' "${rows[@]}"
} >>"${summary}"

exit "${verdict}"
