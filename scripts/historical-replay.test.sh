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
    'finding=' \
    'while (($#)); do' \
    '  if [[ "$1" == --out ]]; then out=$2; shift 2' \
    '  elif [[ "$1" == --finding ]]; then finding=$2; shift 2' \
    '  else shift; fi' \
    'done' \
    'if [[ -n ${FAKE_EXPECT_FINDING:-} && "$finding" != "$FAKE_EXPECT_FINDING" ]]; then' \
    '  printf "wrong finding: %s\n" "$finding"; exit 42' \
    'fi' \
    'mkdir -p "$out"' \
    'cp "$FAKE_REPORT" "$out/report.json"' \
    'if [[ -n ${FAKE_OUTPUT:-} ]]; then printf "%s\n" "$FAKE_OUTPUT"; fi' \
    'exit "${FAKE_EXIT_STATUS:-0}"' \
    >"${work}/tools/harmony"
chmod +x "${work}/tools/harmony"
printf x >"${work}/guest/bzImage"
printf x >"${work}/guest/initramfs-oci.cpio.gz"
mkdir -p "${work}/guest/uml"
printf x >"${work}/guest/uml/profile.json"
printf x >"${work}/guest/uml/linux"
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'while (($#)); do' \
    '  if [[ "$1" == -- ]]; then shift; exec "$@"; fi' \
    '  shift' \
    'done' >"${work}/guest/uml/harmony-uml-qualify"
chmod +x "${work}/guest/uml/harmony-uml-qualify"
printf x >"${work}/oci-images/pgcic-14.3.oci"
printf '[{"Wait":50}]\n' >"${work}/input.json"
mkdir -p "${work}/search"
jq -cn '{mode:"search",bugs:[
    {confirmed:false,actions:[1,2],violations:["case-assertion"],sometimes:["case-evidence"]},
    {confirmed:true,actions:[1,2,3],violations:["other-assertion"],sometimes:["case-evidence"]},
    {confirmed:true,actions:[1],violations:["case-assertion"],sometimes:["case-evidence"]}]}' \
    >"${work}/search/report.json"
reproducer="${work}/search/report.json"

run=$(jq -cn '{run:1,bug:false,stop:"Deadline",state_hash:"abc",violations:[],sometimes:["case-evidence"],actions_applied:1,settle_actions:0,settle_ticks:0,guest_horizons:1,check:null}')
jq -cn --argjson run "${run}" '{mode:"replay",replays:[$run,$run]}' >"${work}/report.json"
summary="${work}/summary.md"

(
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
)
grep -q 'Replay cap: 2 sessions; requested: 2.' "${summary}"
jq -e '.mode == "sample" and .repeats == 2 and .oracle == "pass"' \
    "${work}/reports/pgcic.sample-input/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" FAKE_EXIT_STATUS=23 GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
); then
    printf 'FAIL replay masked a nonzero CLI exit\n'
    exit 1
fi
grep -q 'fail: infra-failure (CLI exit 23)' "${work}/reports/pgcic.sample-input/panel-status.json"

uml_run=$(jq -c '.bug = true | .stop = "Assertion"
    | .violations = ["case-assertion"]
    | .state_hash = "" | .state_hash_encoding = "unavailable"' <<<"$run")
jq -cn --argjson run "$uml_run" '{mode:"replay",replays:[$run,$run]}' \
    >"${work}/uml-report.json"
(
    cd "${work}"
    export BACKEND=uml CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/uml-report.json" FAKE_EXIT_STATUS=2
    export FAKE_OUTPUT='error: full UML state identity is unavailable'
    export FAKE_EXPECT_FINDING=3
    export GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" reproduce "${reproducer}"
)
jq -e '.cli_exit_status == 2 and .state_hash_count == "unavailable"
    and (.oracle | startswith("pass: observed replay"))' \
    "${work}/reports/pgcic.reproduce-report/panel-status.json" >/dev/null

if (
    cd "${work}"
    export BACKEND=uml CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/uml-report.json" FAKE_EXIT_STATUS=2
    export GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" reproduce "${reproducer}"
); then
    printf 'FAIL UML replay accepted a CLI exit without the identity refusal\n'
    exit 1
fi
grep -q 'fail: infra-failure (CLI exit 2)' \
    "${work}/reports/pgcic.reproduce-report/panel-status.json"

jq '.replays[0].state_hash_encoding = "engine_digest"' \
    "${work}/uml-report.json" >"${work}/mixed-identity-report.json"
if (
    cd "${work}"
    export BACKEND=uml CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/mixed-identity-report.json" FAKE_EXIT_STATUS=2
    export FAKE_OUTPUT='error: full UML state identity is unavailable'
    export GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" reproduce "${reproducer}"
); then
    printf 'FAIL UML replay accepted a mixed-identity report\n'
    exit 1
fi
grep -q 'fail: infra-failure (CLI exit 2)' \
    "${work}/reports/pgcic.reproduce-report/panel-status.json"

if (
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=3 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" sample "${work}/input.json"
); then
    printf 'FAIL replay cap allowed an over-budget sample\n'
    exit 1
fi

if (
    cd "${work}"
    export CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL RAM_MIB=128
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence
    export REPLAY_REPEATS=2 MAX_REPLAY_SESSIONS=2 REPLAY_TIMEOUT_SECONDS=10
    export FAKE_REPORT="${work}/report.json" GITHUB_STEP_SUMMARY="${summary}"
    "${here}/historical-replay.sh" discovery "${work}/input.json"
); then
    printf 'FAIL replay still accepts a fixed-version comparison mode\n'
    exit 1
fi
printf 'historical replay budget checks passed\n'
