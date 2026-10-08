#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
binary=${1:?Harmony binary required}
config=${2:?recipe with admitted image and UML paths required}
out=${3:?fresh evidence directory required}
repo=$(cd "$(dirname "$0")/../../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
cp "$config" "$out/harmony.toml"
cd "$out"
"$binary" prepare --json > admission.json
"$binary" debug run --actions "$repo/demos/raft/workload/scenario.json" --name failure > failure.log 2>&1 || test "$?" -eq 1
"$binary" branch failure --step 1 --exec '/app/control trace' --name investigation > investigation.log 2>&1 || test "$?" -eq 1
"$binary" branch failure --step 1 --exec '/app/control require-quorum' --name quorum > quorum.log 2>&1
"$binary" debug replay failure --repeat 2 --name verified > verified.log 2>&1
printf 'touch /tmp/browser-shell-proof\nexit\n' | "$binary" branch failure --step 1 --shell --name inspected > shell.log 2>&1
"$binary" branch inspected --exec 'test -f /tmp/browser-shell-proof && echo RESTORED_SHELL' --stop --name shell-copy > shell-copy.log 2>&1
"$binary" show shell-copy --logs --contains RESTORED_SHELL > shell-proof.log
grep -q RESTORED_SHELL shell-proof.log
python3 - <<'PY'
import hashlib, json
from pathlib import Path
for name in ('failure', 'investigation', 'quorum'):
    run = Path('.harmony/runs') / name
    report = json.loads((run / 'report.json').read_text())
    replay = report['replays'][0]
    assert replay['violations'] == ([] if name == 'quorum' else ['raft-ack-durability'])
    assert replay['settle_actions'] == 0
    Path(name + '.json').write_bytes((run / 'report.json').read_bytes())
    if name == 'investigation':
        assert any('replica acknowledgments=1; required=2' in t['console'] for t in replay['timeline'])
print('PASS: failure, traced rewind, quorum counterfactual, two fresh replays, and restored shell mutation')
PY
