#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Drive the workspace verbs over a real recorded finding on a KVM host.
#
#   historical-investigate.sh <input.json>
#
# Replaying the case's committed input leaves a workspace holding the finding it
# reproduced. This script then does what an investigation does — fork before the
# failure, run forward, execute a diagnostic command, read the evidence, export
# — and checks the contracts that only a live guest can settle:
#
#   * a cold fork with no intervention reaches the recorded finding's own state
#     hash and virtual moment, in a separate process from the one that recorded
#     it;
#   * splitting that advance across two commands changes neither;
#   * `exec` marks its branch modified, retains its output, and leaves the
#     recorded finding untouched;
#   * a retried request id returns the committed result instead of running the
#     command again.
#
# A failure prints what differed. The step summary is written either way.
set -euo pipefail

input=$1

: "${CASE_ID:?}" "${HORIZON_MS:?}" "${RAM_MIB:?}" "${VULNERABLE_VERSION:?}"
: "${ORACLE_ASSERTION:?}"
knobs=${KNOBS:-}

harmony=${PWD}/tools/harmony
agent=${PWD}/tools/fault-agent
kernel=${PWD}/guest/bzImage-faultlab
base_initramfs=${PWD}/guest/initramfs.cpio.gz
chmod +x "${harmony}" "${agent}"
test -f "${input}"

workspace=reports/${CASE_ID}.investigation
rm -rf "${workspace}"
mkdir -p reports
summary=${GITHUB_STEP_SUMMARY:-/dev/null}
rows=()
verdict=0

record() {
    local name=$1 ok=$2 evidence=$3
    rows+=("| ${name} | $([[ ${ok} == 0 ]] && echo pass || echo FAIL) | ${evidence} |")
    [[ ${ok} == 0 ]] || verdict=1
}

# Each verb is a separate process, which is the point: guest time is frozen
# between them and every command restores what it needs from the workspace.
w() { "${harmony}" -w "${workspace}" --json "$@"; }

echo "::group::replay the committed input into a workspace"
timeout -k 30 1800 "${harmony}" search --package faults \
    "oci-images/pgcic-${VULNERABLE_VERSION}.oci" \
    --backend consonance \
    --kernel "${kernel}" \
    --base-initramfs "${base_initramfs}" \
    --fault-agent "${agent}" \
    --replay "${input}" \
    --repeat 1 \
    --horizon-ms "${HORIZON_MS}" \
    --ram-mib "${RAM_MIB}" \
    --knobs "${knobs}" \
    --out "${workspace}" 2>&1 | tee "reports/${CASE_ID}.investigation.console.txt"
echo "::endgroup::"

findings=$(w findings)
echo "${findings}" >"reports/${CASE_ID}.findings.json"
count=$(jq '.findings | length' <<<"${findings}")
[[ ${count} == 1 ]] && record "the replay left one finding" 0 "${count} finding(s)" \
    || record "the replay left one finding" 1 "${count} finding(s)"
if [[ ${count} != 1 ]]; then
    printf '%s\n' "## Investigation acceptance" "" \
        "| check | result | evidence |" "|---|---|---|" "${rows[@]}" >>"${summary}"
    exit 1
fi

violated=$(jq -r '.findings[0].violations | join(",")' <<<"${findings}")
[[ ",${violated}," == *",${ORACLE_ASSERTION},"* ]] \
    && record "it violated the case's oracle" 0 "assertion ${violated}" \
    || record "it violated the case's oracle" 1 "assertion ${violated}, wanted ${ORACLE_ASSERTION}"

recorded_hash=$(jq -r '.findings[0].moment as $m | $m' <<<"${findings}")
inspected=$(w inspect bug-1)
echo "${inspected}" >"reports/${CASE_ID}.inspect.json"
target_hash=$(jq -r '.state_hash // ""' <<<"${inspected}")
target_time=$(jq -r '.virtual_time_nanos' <<<"${inspected}")
[[ -n ${target_hash} ]] \
    && record "the finding carries a state hash" 0 "${target_hash}" \
    || record "the finding carries a state hash" 1 "none recorded at ${recorded_hash}"

meaning=$(jq -r '.properties[0].meaning // ""' <<<"${inspected}")
[[ -n ${meaning} ]] \
    && record "inspection names what the property claims" 0 "${meaning}" \
    || record "inspection names what the property claims" 1 "no declared meaning"

rewind_ns=$((HORIZON_MS * 1000000 * 3))

# Without --extend the advance stops at the source's recorded end, which is
# the finding's own moment, so a generous bound still lands exactly there.
echo "::group::cold fork and one long advance"
w fork bug-1 --rewind "${rewind_ns}ns" --name whole >"reports/${CASE_ID}.fork-whole.json"
w run whole --for "$((rewind_ns * 2))ns" >"reports/${CASE_ID}.run-whole.json"
echo "::endgroup::"
whole_hash=$(jq -r '.state_hash // ""' "reports/${CASE_ID}.run-whole.json")
whole_time=$(jq -r '.virtual_time_nanos' "reports/${CASE_ID}.run-whole.json")

echo "::group::the same advance split across two commands"
w fork bug-1 --rewind "${rewind_ns}ns" --name split >"reports/${CASE_ID}.fork-split.json"
w run split --for "$((rewind_ns / 2))ns" >"reports/${CASE_ID}.run-split-1.json"
w run split --for "$((rewind_ns * 2))ns" >"reports/${CASE_ID}.run-split-2.json"
echo "::endgroup::"
split_hash=$(jq -r '.state_hash // ""' "reports/${CASE_ID}.run-split-2.json")
split_time=$(jq -r '.virtual_time_nanos' "reports/${CASE_ID}.run-split-2.json")

[[ -n ${whole_hash} && ${whole_hash} == "${split_hash}" && ${whole_time} == "${split_time}" ]] \
    && record "splitting an advance changes nothing" 0 "${whole_hash} at ${whole_time}ns" \
    || record "splitting an advance changes nothing" 1 \
       "whole ${whole_hash}@${whole_time} vs split ${split_hash}@${split_time}"

[[ ${whole_time} == "${target_time}" ]] \
    && record "the cold fork reached the recorded moment" 0 "${whole_time}ns" \
    || record "the cold fork reached the recorded moment" 1 \
       "reached ${whole_time}ns, recorded ${target_time}ns"

echo "::group::a guest command and its retry"
w exec whole --within 1s --request-id diagnostic-1 -- \
    sh -c 'cat /run/amcheck.*.out' >"reports/${CASE_ID}.exec.json" || true
w exec whole --within 1s --request-id diagnostic-1 -- \
    sh -c 'cat /run/amcheck.*.out' >"reports/${CASE_ID}.exec-retry.json" || true
echo "::endgroup::"
history=$(jq -r '.history // ""' "reports/${CASE_ID}.exec.json")
[[ ${history} == modified ]] \
    && record "exec marks its branch modified" 0 "${history}" \
    || record "exec marks its branch modified" 1 "history: ${history:-none}"

replayed=$(jq -r '.replayed_request // false' "reports/${CASE_ID}.exec-retry.json")
first_moment=$(jq -r '.moment // ""' "reports/${CASE_ID}.exec.json")
retry_moment=$(jq -r '.moment // ""' "reports/${CASE_ID}.exec-retry.json")
[[ ${replayed} == true && ${first_moment} == "${retry_moment}" ]] \
    && record "a retried request returns its committed result" 0 "${retry_moment}" \
    || record "a retried request returns its committed result" 1 \
       "replayed=${replayed} ${first_moment} vs ${retry_moment}"

after=$(w inspect bug-1)
[[ $(jq -cS '.properties, .virtual_time_nanos, .state_hash' <<<"${inspected}") \
   == $(jq -cS '.properties, .virtual_time_nanos, .state_hash' <<<"${after}") ]] \
    && record "the recorded finding is unchanged" 0 "same property, moment and hash" \
    || record "the recorded finding is unchanged" 1 "the finding moved"

echo "::group::export"
w export bug-1 --out "reports/${CASE_ID}.export" --evidence \
    >"reports/${CASE_ID}.export.json"
echo "::endgroup::"
[[ -s "reports/${CASE_ID}.export/reproducer.json" \
   && -d "reports/${CASE_ID}.export/evidence" ]] \
    && record "export separates the reproducer from the evidence" 0 \
       "reproducer.json and evidence/" \
    || record "export separates the reproducer from the evidence" 1 "missing output"

{
    echo "## Investigation acceptance"
    echo
    echo "| check | result | evidence |"
    echo "|---|---|---|"
    printf '%s\n' "${rows[@]}"
} >>"${summary}"

exit "${verdict}"
