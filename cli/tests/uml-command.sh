#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
binary=${HARMONY_BINARY:-$repo/target/release/harmony}
image=${1:?usage: uml-command.sh IMAGE PROFILE INITRAMFS EVIDENCE_DIRECTORY}
profile=${2:?UML profile required}
initramfs=${3:?base initramfs required}
evidence=${4:?evidence directory required}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
runtime=$(python3 - "$profile" "$initramfs" <<'PYTHON'
import json, sys
print('[runner]\nkind = "consonance"\nbackend = "uml"\n[runner.options]')
print('uml_profile = ' + json.dumps(sys.argv[1]))
print('base_initramfs = ' + json.dumps(sys.argv[2]))
print('ram_mib = 1024')
PYTHON
)
"$binary" debug run "$image" --config-toml "$runtime" --for 60s --out "$evidence/original" -- /bin/true
"$binary" debug replay "$evidence/original" --out "$evidence/replayed"
"$binary" debug replay "$evidence/replayed" --out "$evidence/replayed-again"
python3 - "$evidence" <<'PY'
import hashlib
import json
import sys
from pathlib import Path
root = Path(sys.argv[1])
records = []
for name in ('original', 'replayed', 'replayed-again'):
    path = root / name
    record = json.loads((path / 'run.json').read_text())
    assert record['container_rc'] == 0
    assert record['serial_sha256'] == hashlib.sha256((path / 'serial.log').read_bytes()).hexdigest()
    records.append(record)
for field in ('application_sha256', 'uml_events_sha256', 'steps', 'terminal'):
    assert len({record[field] for record in records}) == 1, field
records[0]['application_sha256'] = '0' * 64
(root / 'original/run.json').write_text(json.dumps(records[0]))
PY
status=0
"$binary" debug replay "$evidence/original" --out "$evidence/diverged" > "$evidence/diverged.txt" 2>&1 || status=$?
test "$status" -eq 2
grep -q 'replay diverged' "$evidence/diverged.txt"
