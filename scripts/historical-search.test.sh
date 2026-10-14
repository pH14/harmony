#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exercise search status handling without requiring Linux/KVM.
# shellcheck disable=SC2030,SC2031
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT
mkdir -p "${work}/tools" "${work}/guest" "${work}/oci-images" "${work}/case"

cat >"${work}/tools/harmony" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
out=
while (($#)); do
    if [[ "$1" == --out ]]; then out=$2; shift 2; else shift; fi
done
mkdir -p "$out"
cp "$FAKE_REPORT" "$out/report.json"
cp "$FAKE_SUMMARY" "$out/campaign-summary.json"
count=$(jq -r '(.bugs // []) | length' "$FAKE_REPORT")
for ((index=1; index<=count; index++)); do
    if [[ ${FAKE_OMIT_BUG:-} == "$index" ]]; then continue; fi
    jq --argjson index "$index" '.bugs[$index - 1]' "$FAKE_REPORT" >"$out/bug-${index}.json"
done
exit "${FAKE_EXIT_STATUS:-0}"
EOF
chmod +x "${work}/tools/harmony"
printf x >"${work}/guest/bzImage"
printf x >"${work}/guest/initramfs-oci.cpio.gz"
printf x >"${work}/oci-images/pgcic-14.3.oci"
printf '%s\n' '{"id":"pgcic"}' >"${work}/case/case.json"

confirmed_replay=$(jq -cn '{run:1,bug:true,violations:["case-assertion"],sometimes:["case-evidence"],
    actions_applied:1,settle_actions:0,settle_ticks:0,guest_horizons:1}')
confirmed_bug=$(jq -cn --argjson replay "${confirmed_replay}" '{execution:12,
    actions:[{"Wait":50}], stop:"Assertion", violations:["case-assertion"], sometimes:["case-evidence"],
    state_hash:"abc", confirmed:true, replay:$replay}')

jq -cn --argjson bug "${confirmed_bug}" '{mode:"search", package:"faults", workers:2,
    executions:2, execution_ticks:2, execution_failures:0, bug_found:true,
    bugs:[$bug]}' >"${work}/report.json"
jq -cn '{watchdog_cutoffs:3, execution_failures:0, executions:2,
    execution_ticks:2}' >"${work}/summary.json"
jq -cn --argjson bug "${confirmed_bug}" '{mode:"search", package:"faults", executions:2,
    execution_ticks:2, execution_failures:1, bug_found:true, bugs:[$bug]}' \
    >"${work}/failure-report.json"
jq -cn '{watchdog_cutoffs:0, execution_failures:1, executions:2,
    execution_ticks:2}' >"${work}/failure-summary.json"
jq -cn '{mode:"search", package:"faults", executions:2,
    execution_ticks:2, execution_failures:0, bug_found:false, bugs:[]}' \
    >"${work}/miss-report.json"
jq -cn --argjson bug "${confirmed_bug}" '{mode:"search", package:"faults", executions:2,
    execution_ticks:2, execution_failures:0, bug_found:true,
    bugs:[($bug | .replay.actions_applied=0)]}' \
    >"${work}/bad-replay-report.json"
jq -cn --argjson bug "${confirmed_bug}" '{mode:"search", package:"faults", workers:2,
    executions:2, execution_ticks:2, execution_failures:0, bug_found:true,
    bugs:[($bug | .replay.settle_actions=[])]}' \
    >"${work}/invalid-findings-report.json"

run_search() {
    local exit_status=${1:-0}
    (
        cd "${work}"
        export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
        export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
        export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
        export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
        export FAKE_REPORT="${work}/report.json" FAKE_SUMMARY="${work}/summary.json"
        export FAKE_EXIT_STATUS=${exit_status} GITHUB_STEP_SUMMARY="${work}/summary.md"
        "${here}/historical-search.sh"
    )
}

run_search
jq -e '.watchdog_cutoffs == 3 and .cli_exit_status == 0 and .workers == 2 and
       .execution_failures == 0 and .oracle == "pass" and
       .finding == 1 and (.reproducer | endswith("/bug-1.json")) and
       .execution_status == "completed_with_watchdog_cutoffs"' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

RUN_KEY=pgcic-seed7 run_search
jq -e '.oracle == "pass"' \
    "${work}/reports/pgcic-seed7/pgcic.search/panel-status.json" >/dev/null
test -f "${work}/reports/pgcic-seed7/pgcic.search.console.txt"

jq -cn --argjson bug "${confirmed_bug}" '{mode:"search", package:"faults", workers:2,
    executions:2, execution_ticks:2, execution_failures:0, bug_found:true,
    bugs:[($bug | .violations=["other-assertion"]), $bug]}' \
    >"${work}/later-case-report.json"
(
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/later-case-report.json" FAKE_SUMMARY="${work}/summary.json"
    export GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
)
jq -e '.oracle == "pass" and .finding == 2
    and (.reproducer | endswith("/bug-2.json"))' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if run_search 23; then
    printf 'FAIL search masked a nonzero CLI exit\n'
    exit 1
fi
jq -e '.watchdog_cutoffs == 3 and .cli_exit_status == 23 and
       .execution_failures == 0 and
       .execution_status == "infra_failure" and
       (.oracle | startswith("fail: infra-failure"))' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/failure-report.json" FAKE_SUMMARY="${work}/failure-summary.json"
    export FAKE_EXIT_STATUS=0 GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
); then
    printf 'FAIL search accepted an injected execution failure\n'
    exit 1
fi
jq -e '.watchdog_cutoffs == 0 and .execution_failures == 1 and
       .cli_exit_status == 0 and .execution_status == "infra_failure" and
       (.oracle | startswith("fail: infra-failure"))' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/miss-report.json" FAKE_SUMMARY="${work}/summary.json"
    export FAKE_EXIT_STATUS=0 GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
); then
    printf 'FAIL search reported a miss as success\n'
    exit 1
fi
jq -e '.oracle == "fail: search-miss" and .execution_status == "completed_with_watchdog_cutoffs"
    and .finding == null and .reproducer == null' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/bad-replay-report.json" FAKE_SUMMARY="${work}/summary.json"
    export GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
); then
    printf 'FAIL search accepted a finding with incomplete replay evidence\n'
    exit 1
fi
jq -e '.finding == null and .reproducer == null and .oracle == "fail: replay-mismatch"' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/invalid-findings-report.json" FAKE_SUMMARY="${work}/summary.json"
    export GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
); then
    printf 'FAIL search accepted malformed findings\n'
    exit 1
fi
jq -e '.finding == null and .reproducer == null and .execution_status == "infra_failure"
    and (.oracle | startswith("fail: infra-failure"))' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

if (
    cd "${work}"
    export CASE_DIR=case CASE_ID=pgcic SOFTWARE_NAME=PostgreSQL
    export WORKLOAD_VERSION=14.3 IMAGE_PREFIX=pgcic RAM_MIB=128
    export SEED=1 EXECUTIONS=2 WALL_MINUTES=1
    export ORACLE_ASSERTION=case-assertion ORACLE_EVIDENCE=case-evidence KNOBS=
    export FAKE_REPORT="${work}/report.json" FAKE_SUMMARY="${work}/summary.json"
    export FAKE_OMIT_BUG=1 GITHUB_STEP_SUMMARY="${work}/summary.md"
    "${here}/historical-search.sh"
); then
    printf 'FAIL search accepted a missing selected bug report\n'
    exit 1
fi
jq -e '.finding == null and .reproducer == null and .execution_status == "infra_failure"
    and (.oracle | startswith("fail: infra-failure"))' \
    "${work}/reports/pgcic.search/panel-status.json" >/dev/null

printf 'historical search status checks passed\n'
