#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Validate the current case roster and its runnable/deferred matrix split.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
manifest=${here}/historical-manifest.py

python3 "${manifest}" --check
all=$(python3 "${manifest}" --matrix)
runnable=$(python3 "${manifest}" --runnable-matrix)
search=$(python3 "${manifest}" --search-matrix)

test "$(jq '.include | length' <<<"${all}")" -eq 2
test "$(jq -r '[.include[] | select(.ci_status == "runnable")] | length' <<<"${all}")" -eq 2
test "$(jq -r '.include[] | select(.id == "postgres-cic-corruption") | .search_timeout_minutes' <<<"${all}")" -eq 170
test "$(jq -r '.include[] | select(.id == "etcd-3.5-inconsistency") | .search_timeout_minutes' <<<"${all}")" -eq 320
test "$(jq '.include | length' <<<"${runnable}")" -eq 2
test "$(jq -r '[.include[].id] | sort | join(",")' <<<"${runnable}")" = etcd-3.5-inconsistency,postgres-cic-corruption
test "$(jq '.include | length' <<<"${search}")" -eq 3
test "$(jq -r '[.include[] | select(.id == "postgres-cic-corruption") | .arm] | .[0]' <<<"${search}")" = vulnerable
test "$(jq -r '[.include[] | select(.id == "etcd-3.5-inconsistency") | .planned_replay_sessions] | unique | .[0]' <<<"${search}")" -eq 1

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
case["ci"]["replay_max_sessions"] = 2
try:
    module.validate(case_path, case)
except SystemExit as error:
    assert "aggregate replay count" in str(error)
else:
    raise SystemExit("manifest accepted a replay cap below its aggregate plan")
PY

printf 'historical manifest checks passed\n'
