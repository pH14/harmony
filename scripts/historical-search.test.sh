#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exercise search status handling without requiring Linux/KVM.
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
exit "${FAKE_EXIT_STATUS:-0}"
EOF
chmod +x "${work}/tools/harmony"
printf x >"${work}/guest/bzImage-faultlab"
printf x >"${work}/guest/initramfs.cpio.gz"
printf x >"${work}/oci-images/pgcic-14.3.oci"
printf '%s\n' '{"id":"pgcic"}' >"${work}/case/case.json"

jq -cn '{mode:"search", package:"faults", executions:2,
    horizons_clocked:2, bug_found:false, bugs:[]}' >"${work}/report.json"
jq -cn '{watchdog_cutoffs:3, executions:2, horizons:2}' >"${work}/summary.json"

run_search() {
    local exit_status=${1:-0}
    (
        cd "${work}"
        export CASE_DIR=case CASE_ID=pgcic ARM=control SOFTWARE_NAME=PostgreSQL
        export WORKLOAD_VERSION=14.4 IMAGE_PREFIX=pgcic HORIZON_MS=500 RAM_MIB=128
        export SEED=1 WORKERS=1 ACTIONS=4 EXECUTIONS=2 WALL_MINUTES=1
        export ORACLE_ASSERTION=2 ORACLE_EVIDENCE=24 KNOBS=
        export FAKE_REPORT="${work}/report.json" FAKE_SUMMARY="${work}/summary.json"
        export FAKE_EXIT_STATUS=${exit_status} GITHUB_STEP_SUMMARY="${work}/summary.md"
        "${here}/historical-search.sh"
    )
}

run_search
jq -e '.watchdog_cutoffs == 3 and .cli_exit_status == 0 and
       .execution_status == "completed_with_watchdog_cutoffs"' \
    "${work}/reports/pgcic.control.search/panel-status.json" >/dev/null

if run_search 23; then
    printf 'FAIL search masked a nonzero CLI exit\n'
    exit 1
fi
jq -e '.watchdog_cutoffs == 3 and .cli_exit_status == 23 and
       .execution_status == "infra_failure" and
       (.oracle | startswith("fail: infra-failure"))' \
    "${work}/reports/pgcic.control.search/panel-status.json" >/dev/null

printf 'historical search status checks passed\n'
