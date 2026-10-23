#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: check-recovery.sh image evidence-directory [config.toml]}
evidence=${2:?evidence-directory required}
recipe=$(cd "$(dirname "$0")" && pwd)
config=${3:-$recipe/harmony.toml}
binary=${HARMONY_BINARY:-target/release/harmony}
"$binary" prepare "$image"
"$binary" debug run "$image" --config "$config" --seed 17 --for 20m \
 --actions "$recipe/recovery.json" --out "$evidence/run"
python3 - "$evidence/run/report.json" <<'PY'
import json
import sys
report = json.load(open(sys.argv[1]))
assert not report['bug_found']
assert not report['never_satisfied']
assert report['execution_failures'] == 0
assert report['watchdog_cutoffs'] == 0
assert report['replays']
for replay in report['replays']:
    assert not replay['violations']
    assert replay['check']['pending_faults'] == 0
    assert replay['check']['undecided_keys'] == 0
    assert 'harness: a check read succeeded after a fault' in replay['check']['points']
PY
"$binary" debug replay "$evidence/run" --out "$evidence/replay"
