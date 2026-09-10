#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Judge one historical-bug report against the case's own oracle.
#
#   historical-oracle.sh replay <report.json> <arm> <repeats> <actions>
#   historical-oracle.sh search <report.json> <arm>
#
# ORACLE_ASSERTION is the case's `oracle.assertion` id and ORACLE_EVIDENCE its
# `oracle.evidence` id: the assertion whose violation is this bug, and the
# reachable point the detector publishes whenever it reached a verdict. Both
# arms are judged on those two ids rather than on the generic bug flag, which
# any crash or any other assertion would also raise.
#
# A verdict line goes to stdout: `pass`, or `fail: <reason>`.
set -euo pipefail

: "${ORACLE_ASSERTION:?}" "${ORACLE_EVIDENCE:?}"

mode=$1
report=$2
arm=$3

case "${arm}" in
    vulnerable | control) ;;
    *) echo "fail: unknown arm ${arm}"; exit 2 ;;
esac

if [[ ! -s "${report}" ]]; then
    echo "fail: no report at ${report}"
    exit 1
fi

# Each rule is one jq test with the words that go in the verdict when it does
# not hold, so a failing run names the missing evidence rather than a boolean.
rules=()
add_rule() { rules+=("$1"$'\n'"$2"); }

case "${mode}" in
    replay)
        repeats=$4
        actions=$5
        add_rule 'the report is not a replay report' '.mode == "replay"'
        add_rule "the report holds no ${repeats} replay runs" \
            "(.replays | length) == ${repeats}"
        # A run whose guest horizons fall short of the actions it applied took
        # part of its prefix from a snapshot an earlier run cached, so it never
        # executed the recorded actions end to end.
        add_rule 'a run reused a cached prefix instead of executing its actions' \
            'all(.replays[]; .guest_horizons == .actions_applied)'
        add_rule 'a run applied no action at all' \
            'all(.replays[]; .actions_applied >= 1)'
        add_rule "a run applied more than ${actions} actions" \
            "all(.replays[]; .actions_applied <= ${actions})"
        # Silence is only evidence when the detector ran: the workload's oracle
        # publishes this point on every verdict it reached, corrupt or clean.
        add_rule "a run never reached the oracle's verdict (point ${ORACLE_EVIDENCE})" \
            "all(.replays[]; .sometimes | index(${ORACLE_EVIDENCE}) != null)"
        if [[ "${arm}" == vulnerable ]]; then
            add_rule "a run did not violate assertion ${ORACLE_ASSERTION}" \
                "all(.replays[]; .violations | index(${ORACLE_ASSERTION}) != null)"
        else
            add_rule 'a control run reported a bug' 'all(.replays[]; .bug == false)'
            add_rule 'a control run violated an assertion' \
                'all(.replays[]; (.violations | length) == 0)'
            add_rule "a control run stopped before its ${actions} actions were applied" \
                "all(.replays[]; .actions_applied == ${actions})"
        fi
        ;;
    search)
        add_rule 'the report is not a search report' '.mode == "search"'
        if [[ "${arm}" == vulnerable ]]; then
            add_rule "no confirmed bug carries assertion ${ORACLE_ASSERTION} and the oracle's verdict evidence" \
                "any(.bugs[]; .confirmed
                     and (.violations | index(${ORACLE_ASSERTION}) != null)
                     and (.sometimes | index(${ORACLE_EVIDENCE}) != null)
                     and (.replay != null)
                     and (.replay.violations | index(${ORACLE_ASSERTION}) != null)
                     and (.replay.sometimes | index(${ORACLE_EVIDENCE}) != null)
                     and (.replay.actions_applied == (.actions | length))
                     and (.replay.guest_horizons == .replay.actions_applied))"
            add_rule 'the report records no bug' '.bug_found == true'
        else
            add_rule "the control campaign never reached the oracle's verdict (point ${ORACLE_EVIDENCE})" \
                "(((.campaign_milestones.sometimes // 0) / pow(2; ${ORACLE_EVIDENCE}) | floor) % 2) == 1"
            add_rule 'the control campaign reported a bug' '.bug_found == false'
            add_rule 'the control campaign archived a bug' '(.bugs | length) == 0'
        fi
        ;;
    *)
        echo "fail: unknown mode ${mode}"
        exit 2
        ;;
esac

for rule in "${rules[@]}"; do
    reason=${rule%%$'\n'*}
    filter=${rule#*$'\n'}
    if ! jq -e "${filter}" "${report}" >/dev/null; then
        echo "fail: ${reason}"
        exit 1
    fi
done

echo pass
