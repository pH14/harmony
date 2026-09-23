#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Validate the current case roster and its runnable/deferred matrix split.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
manifest=${here}/historical-manifest.py

python3 "${manifest}" --check
all=$(python3 "${manifest}" --matrix)
runnable=$(python3 "${manifest}" --runnable-matrix)

test "$(jq '.include | length' <<<"${all}")" -eq 2
test "$(jq -r '[.include[] | select(.ci_status == "runnable")] | length' <<<"${all}")" -eq 2
test "$(jq -r '.include[] | select(.id == "postgres-cic-corruption") | .job_timeout_minutes' <<<"${all}")" -eq 230
test "$(jq -r '.include[] | select(.id == "etcd-3.5-inconsistency") | .job_timeout_minutes' <<<"${all}")" -eq 320
test "$(jq -r '.include[] | select(.id == "postgres-cic-corruption") | .display_name' <<<"${all}")" = "PostgreSQL Index Corruption"
test "$(jq -r '.include[] | select(.id == "etcd-3.5-inconsistency") | .display_name' <<<"${all}")" = "etcd Data Inconsistency"
test "$(jq -r '.include[] | select(.id == "postgres-cic-corruption") | .workload_version' <<<"${all}")" = 14.3
test "$(jq -r '[.include[] | has("arm")] | any' <<<"${all}")" = false
test "$(jq '.include | length' <<<"${runnable}")" -eq 2
test "$(jq -r '[.include[].id] | sort | join(",")' <<<"${runnable}")" = etcd-3.5-inconsistency,postgres-cic-corruption
test "$(jq -r '.include[] | select(.id == "postgres-cic-corruption") | .planned_replay_sessions' <<<"${runnable}")" -eq 2
test "$(jq -r '.include[] | select(.id == "etcd-3.5-inconsistency") | .planned_replay_sessions' <<<"${runnable}")" -eq 0

if python3 "${manifest}" --search-matrix >/dev/null 2>&1; then
    printf 'FAIL the manifest still emits a per-arm search matrix\n'
    exit 1
fi
if python3 "${manifest}" --replay-matrix >/dev/null 2>&1; then
    printf 'FAIL the manifest still emits a fixed-version replay matrix\n'
    exit 1
fi

python3 - "${manifest}" <<'PY'
import copy
import importlib.util
import json
import sys
from pathlib import Path

manifest_path = Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("historical_manifest", manifest_path)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
case_path = module.ROOT / "workloads/bugs/historical/postgres-cic-corruption/case.json"
case = json.loads(case_path.read_text())
case["ci"] = copy.deepcopy(case["ci"])
case["ci"]["replay_max_sessions"] = 1
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "aggregate replay count" in str(error)
else:
    raise SystemExit("manifest accepted a replay cap below its aggregate plan")

case = json.loads(case_path.read_text())
case["arms"] = {"vulnerable": {"version": "14.3"}, "control": {"version": "14.4"}}
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "arms are not supported" in str(error)
else:
    raise SystemExit("manifest accepted a fixed-version comparison arm")

case = json.loads(case_path.read_text())
case["ci"] = copy.deepcopy(case["ci"])
case["ci"]["search_arms"] = ["vulnerable", "control"]
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "search_arms is not supported" in str(error)
else:
    raise SystemExit("manifest accepted a declared search arm")

case = json.loads(case_path.read_text())
case["search"] = copy.deepcopy(case["search"])
case["search"]["wall_minutes"] = 340
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "360-minute job limit" in str(error)
else:
    raise SystemExit("manifest accepted a case that cannot finish inside a GitHub job")

case = json.loads(case_path.read_text())
case["ci"] = copy.deepcopy(case["ci"])
del case["ci"]["display_name"]
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "ci.display_name" in str(error)
else:
    raise SystemExit("manifest accepted a runnable case with no scenario job name")
PY

matrix=$(python3 scripts/historical-manifest.py --runnable-matrix --case etcd-3.5-inconsistency --seeds 3,5)
python3 - "${matrix}" <<'PY'
import json
import sys

entries = json.loads(sys.argv[1])["include"]
assert [entry["seed"] for entry in entries] == [3, 5], entries
assert len({entry["run_key"] for entry in entries}) == 2, entries
assert {entry["id"] for entry in entries} == {"etcd-3.5-inconsistency"}, entries
PY
if python3 scripts/historical-manifest.py --runnable-matrix --case no-such-case >/dev/null 2>&1; then
    printf 'manifest accepted an unknown case filter\n' >&2
    exit 1
fi

printf 'historical manifest checks passed\n'
