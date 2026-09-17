#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Judge one historical case report.
#
#   historical-oracle.sh search <report.json>
#   historical-oracle.sh sample <report.json> <runs> <actions>
#
# Search and sample replay have different evidence contracts. A search miss
# means no candidate was found; a candidate whose fresh replay did not verify is
# reported separately. A declared clean sample must reach the detector on the
# same affected version and stay free of the case's assertion.
set -euo pipefail

: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

mode=${1:?mode is required}
report=${2:?report is required}

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
        [[ "${raw}" == true ]] || fail search-miss

        # The package's campaign already performs one fresh replay for each
        # recorded bug. Its summary must carry the same detector evidence and
        # prove that no cached prefix answered it.
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
                and (.replay.guest_horizons == (.replay.actions_applied + .replay.settle_actions))
                and (.replay.settle_ticks >= .replay.settle_actions)
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
        ;;

    sample)
        repeats=${3:?replay count is required}
        actions=${4:?action count is required}
        is_uint "${repeats}" || fail infra-failure
        is_uint "${actions}" || fail infra-failure
        check infra-failure '.mode == "replay"'
        check infra-failure "(.replays | length) == ${repeats}"
        check infra-failure \
            "all(.replays[]; (.guest_horizons == (.actions_applied + .settle_actions))\
                and (.actions_applied == ${actions})\
                and (.settle_actions >= 0)\
                and (.settle_ticks >= .settle_actions))"
        if jq -e --argjson assertion "${assertion}" \
            'any(.replays[]; .bug == true or ((.violations // []) | length) > 0)' \
            "${report}" >/dev/null 2>&1; then
            fail sample-violation
        fi
        check replay-inconclusive \
            "all(.replays[]; (.sometimes // []) | index(${evidence}) != null)"
        check infra-failure 'all(.replays[]; has("check"))'
        check replay-inconclusive \
            "all(.replays[]; .check == null or (
                .check.run > 0
                and .check.start_generation == .check.disturbance_generation
                and .check.end_generation == .check.disturbance_generation
                and .check.pending_faults == 0
                and ((.check.points // []) | index(${evidence})) != null
            ))"
        check replay-inconclusive \
            'all(.replays[]; .bug == false and ((.violations // []) | length) == 0)'
        check replay-mismatch '[.replays[].state_hash] | unique | length == 1'
        echo pass
        ;;
    *)
        fail "infra-failure (unknown mode ${mode})"
        ;;
esac
