#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exercise replay-session budgeting without requiring Linux/KVM.
# shellcheck disable=SC2016,SC2030,SC2031
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT
mkdir -p "${work}/tools" "${work}/guest" "${work}/oci-images"

printf '%s\n' \
    '#!/usr/bin/env bash' \
    'set -euo pipefail' \
    'out=' \
    'while (($#)); do' \
    '  if [[ "$1" == --out ]]; then out=$2; shift 2; else shift; fi' \
    'done' \
    'mkdir -p "$out"' \
    'cp "$FAKE_REPORT" "$out/report.json"' \
    'exit "${FAKE_EXIT_STATUS:-0}"' \
    >"${work}/tools/harmony"
chmod +x "${work}/tools/harmony"
printf x >"${work}/guest/bzImage"
printf x >"${work}/guest/initramfs-oci.cpio.gz"
printf x >"${work}/oci-images/pgcic-14.3.oci"
printf x >"${work}/oci-images/pgcic-14.4.oci"
printf '["Wait"]\n' >"${work}/input.json"

run=$(jq -cn '{run:1,bug:false,stop:"Deadline",state_hash:"abc",violations:[],sometimes:[24],actions_applied:1,guest_horizons:1}')
jq -cn --argjson run "${run}" '{mode:"replay",replays:[$run,$run]}' >"${work}/report.json"
summary="${work}/summary.md"

(
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL HORIZON_MS=500 RAM_MIB=128
    export VULNERABLE_VERSION=14.3 CONTROL_VERSION=14.4 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24 REPLAY_ARMS=vulnerable
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
)
grep -q 'Replay cap: 2 sessions; requested: 2.' "${summary}"

if (
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL HORIZON_MS=500 RAM_MIB=128
    export VULNERABLE_VERSION=14.3 CONTROL_VERSION=14.4 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24 REPLAY_ARMS=vulnerable
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" FAKE_EXIT_STATUS=23 GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
); then
    printf 'FAIL replay masked a nonzero CLI exit\n'
    exit 1
fi
grep -q 'fail: infra-failure (CLI exit 23)' "${work}/reports/pgcic.vulnerable.sample/panel-status.json"

if (
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL HORIZON_MS=500 RAM_MIB=128
    export VULNERABLE_VERSION=14.3 CONTROL_VERSION=14.4 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24 REPLAY_ARMS=vulnerable,control
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=3 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
); then
    printf 'FAIL replay cap allowed an over-budget sample\n'
    exit 1
fi
printf 'historical replay budget checks passed\n'
