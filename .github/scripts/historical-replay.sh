#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Replay one committed action list on both arms of a historical-bug case.
#
#   historical-replay.sh <mode> <input.json>
#
# Every repeat on the vulnerable arm must violate the case's own oracle
# assertion, and every control repeat must reach that oracle's verdict and pass
# it. historical-oracle.sh holds the rule; a crash, another assertion, or a
# detector that never ran does not stand in for either result. Anything else
# fails, including a replay that produced no report at all. The step summary is
# written whether the rule holds or not, because a failing run is the evidence a
# reader most wants to see.
set -euo pipefail

mode=$1
input=$2

: "${CASE_ID:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}"
: "${VULNERABLE_VERSION:?}" "${CONTROL_VERSION:?}" "${IMAGE_PREFIX:?}"
: "${SOFTWARE_NAME:?}"
: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

# The knobs reach the workload on the guest command line, so a replay only
# reproduces a search's conditions when it boots with the same ones.
knobs=${KNOBS:-}

# Two repeats on the arm that is supposed to fire, so a single lucky run cannot
# carry the claim; one on the control, which only has to stay silent.
vulnerable_repeats=${VULNERABLE_REPEATS:-2}
control_repeats=${CONTROL_REPEATS:-1}

harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/guest/bzImage-faultlab
base_initramfs=${PWD}/guest/initramfs.cpio.gz
chmod +x "${harmony}" "${agent}"
test -f "${input}"

oracle=$(dirname "$0")/historical-oracle.sh

# The recorded input bounds how many actions a run may apply; a report that
# claims more ran than the input names is not a replay of this input.
actions=$(jq -r 'if type == "array" then length else (.actions | length) end' "${input}")

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
    if [[ "${status}" -ne 0 ]] || [[ ! -s "${report}" ]]; then
        rows+=("| ${arm} | ${version} | ${repeats} | — | — | ${want} | — | fail: no report (exit ${status}) |")
        verdict=1
        return
    fi

    local violated checked hashes_agree
    violated=$(jq -r --argjson id "${ORACLE_ASSERTION}" \
        '[.replays[] | select(.violations | index($id))] | length' "${report}")
    checked=$(jq -r --argjson id "${ORACLE_EVIDENCE}" \
        '[.replays[] | select(.sometimes | index($id))] | length' "${report}")
    hashes_agree=$(jq -r '[.replays[].state_hash] | unique | length == 1' "${report}")

    # State-hash agreement is reported beside the rule and is not part of it:
    # hosted runners have stock KVM, where the guest can read the host counter
    # directly.
    local ok
    ok=$("${oracle}" replay "${report}" "${arm}" "${repeats}" "${actions}") || verdict=1
    rows+=("| ${arm} | ${version} | ${repeats} | ${violated} | ${checked} | ${want} | ${hashes_agree} | ${ok} |")
}

replay_arm vulnerable "${VULNERABLE_VERSION}" "${vulnerable_repeats}" true
replay_arm control "${CONTROL_VERSION}" "${control_repeats}" false

{
    echo "## ${mode} replay — ${input}"
    echo
    echo "| arm | ${SOFTWARE_NAME} | repeats | runs violating assertion ${ORACLE_ASSERTION} |\
 runs reaching the oracle verdict | violation expected | state hashes agree | verdict |"
    echo "|---|---|---|---|---|---|---|---|"
    printf '%s\n' "${rows[@]}"
} >>"${summary}"

exit "${verdict}"
