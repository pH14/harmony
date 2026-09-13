#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Judge one historical case report.
#
#   historical-oracle.sh search <report.json> <arm>
#   historical-oracle.sh replay <report.json> <arm> <runs> <actions>
#   historical-oracle.sh discovery <report.json> control <runs> <actions>
#   historical-oracle.sh sample <report.json> <arm> <runs> <actions>
#
# Search and replay have different evidence contracts. A search miss means no
# candidate was found; a candidate whose fresh replay did not verify is
# reported separately. A clean control replay must reach the detector, while a
# violation on the fixed arm is an actual replay mismatch.
set -euo pipefail

: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

mode=${1:?mode is required}
report=${2:?report is required}
arm=${3:?arm is required}

case "${arm}" in
    vulnerable|control) ;;
    *) echo "fail: infra-failure (unknown arm ${arm})"; exit 1 ;;
esac

if [[ ! -s "${report}" ]]; then
    echo "fail: infra-failure (no report at ${report})"
    exit 1
fi

is_uint() { [[ "$1" =~ ^[0-9]+$ ]]; }
if ! is_uint "${ORACLE_ASSERTION}" || ! is_uint "${ORACLE_EVIDENCE}"; then
    echo "fail: infra-failure (oracle ids must be non-negative integers)"
    exit 1
fi

fail() {
    echo "fail: $1"
    exit 1
}

check() {
    local reason=$1 filter=$2
    if ! jq -e "${filter}" "${report}" >/dev/null 2>&1; then
        fail "${reason}"
    fi
}

assertion=${ORACLE_ASSERTION}
evidence=${ORACLE_EVIDENCE}

case "${mode}" in
    search)
        check infra-failure '.mode == "search"'

        raw=$(jq -r --argjson id "${assertion}" \
            'any(.bugs[]?; (.violations // [] | index($id)) != null)' "${report}")
        if [[ "${arm}" == vulnerable ]]; then
            [[ "${raw}" == true ]] || fail search-miss

            # The package's campaign already performs one fresh replay for
            # each recorded bug. Its summary must carry the same detector
            # evidence and prove that no cached prefix answered it.
            verified=$(jq -r --argjson assertion "${assertion}" \
                --argjson evidence "${evidence}" '
                any(.bugs[]?;
                    .confirmed == true
                    and ((.violations // []) | index($assertion)) != null
                    and ((.sometimes // []) | index($evidence)) != null
                    and (.replay != null)
                    and (.replay.bug == true)
                    and (((.replay.violations // []) | index($assertion)) != null)
                    and (((.replay.sometimes // []) | index($evidence)) != null)
                    and (.replay.guest_horizons == .replay.actions_applied)
                )' "${report}")
            [[ "${verified}" == true ]] && { echo pass; exit 0; }

            replayed=$(jq -r --argjson assertion "${assertion}" '
                any(.bugs[]?;
                    .confirmed == true
                    and ((.violations // []) | index($assertion)) != null
                    and (.replay != null)
                )' "${report}")
            [[ "${replayed}" == true ]] && fail replay-mismatch
            fail found-but-replay-unverified
        fi

        # A control search is a discovery control only. Its fresh differential
        # replay below proves the detector ran; a campaign hit is still an
        # actual control-arm violation, not a successful search.
        bug_count=$(jq -r '(.bugs // []) | length' "${report}")
        bug_found=$(jq -r '.bug_found == true' "${report}")
        if [[ "${bug_count}" -gt 0 || "${bug_found}" == true ]]; then
            confirmed=$(jq -r 'any(.bugs[]?; .confirmed == true)' "${report}")
            [[ "${confirmed}" == true ]] && fail actual-replay-mismatch
            fail found-but-replay-unverified
        fi
        echo pass
        ;;

    replay|discovery|sample)
        repeats=${4:?replay count is required}
        actions=${5:?action count is required}
        is_uint "${repeats}" || fail infra-failure
        is_uint "${actions}" || fail infra-failure
        check infra-failure '.mode == "replay"'
        check infra-failure "(.replays | length) == ${repeats}"
        check infra-failure \
            "all(.replays[]; (.guest_horizons == .actions_applied)\
                and (.actions_applied >= 1)\
                and (.actions_applied <= ${actions}))"

        case "${mode}:${arm}" in
            replay:vulnerable)
                check replay-inconclusive \
                    "all(.replays[]; (.sometimes // []) | index(${evidence}) != null)"
                check replay-mismatch \
                    "all(.replays[]; (.bug == true)\
                        and (((.violations // []) | index(${assertion})) != null))"
                ;;
            replay:control|discovery:control|sample:*)
                check infra-failure \
                    "all(.replays[]; .actions_applied == ${actions})"
                if jq -e --argjson assertion "${assertion}" \
                    'any(.replays[]; .bug == true or ((.violations // []) | length) > 0)' \
                    "${report}" >/dev/null 2>&1; then
                    fail actual-replay-mismatch
                fi
                check replay-inconclusive \
                    "all(.replays[]; (.sometimes // []) | index(${evidence}) != null)"
                check infra-failure 'all(.replays[]; has("check"))'
                exercised='.check.disturbance_generation > 0'
                [[ "${mode}" == sample ]] && exercised=true
                check replay-inconclusive \
                    "all(.replays[]; .check == null or (
                        .check.run > 0
                        and (${exercised})
                        and .check.start_generation == .check.disturbance_generation
                        and .check.end_generation == .check.disturbance_generation
                        and .check.pending_faults == 0
                        and ((.check.points // []) | index(${evidence})) != null
                    ))"
                check replay-inconclusive \
                    'all(.replays[]; .bug == false and ((.violations // []) | length) == 0)'
                if [[ "${mode}" == sample ]]; then
                    check replay-mismatch '[.replays[].state_hash] | unique | length == 1'
                fi
                ;;
            *)
                fail "infra-failure (discovery requires the control arm)"
                ;;
        esac
        echo pass
        ;;
    *)
        fail "infra-failure (unknown mode ${mode})"
        ;;
esac
