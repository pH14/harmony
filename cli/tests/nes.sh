#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
binary=${HARMONY_BINARY:-$repo/target/release/harmony}
rom=${1:?usage: nes.sh ROM CORE EVIDENCE_DIRECTORY}
core=${2:?QuickNES core required}
evidence=${3:?evidence directory required}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
recipe=$(python3 - "$core" <<'PYTHON'
import json, sys
print('[runner]\nkind = "quicknes"\n[runner.options]\ncore = ' + json.dumps(sys.argv[1]))
PYTHON
)
"$binary" prepare "$rom"
"$binary" search "$rom" --config-toml "$recipe" --executions 4 --out "$evidence/search"
"$binary" replay "$evidence/search" --out "$evidence/replay"
"$binary" branch "$evidence/search" --step 0 --stop --out "$evidence/prefix"
"$binary" replay "$evidence/prefix" --out "$evidence/prefix-copy"
printf '%s\n' '{"actions":[{"buttons":0,"hold_frames":1},{"buttons":0,"hold_frames":1}]}' > "$evidence/input.json"
"$binary" run "$rom" --config-toml "$recipe" --actions "$evidence/input.json" --repeat 2 --out "$evidence/scripted"
"$binary" branch "$evidence/scripted" --step 1 --stop --out "$evidence/nonempty-prefix"
"$binary" search --from "$evidence/nonempty-prefix" --executions 4 --out "$evidence/rooted"
"$binary" resume "$evidence/rooted" --executions 4 --out "$evidence/continued"
"$binary" timeline "$evidence/search" --step 0 --json > "$evidence/timeline.json"
"$binary" findings "$evidence/search" --json > "$evidence/findings.json"
python3 - "$evidence" <<'PYTHON'
import json, sys
from pathlib import Path
root=Path(sys.argv[1])
def manifest(name): return json.loads((root/name/'manifest.json').read_text())
assert len(manifest('scripted')['payload']['repeats']) == 2
assert manifest('search')['payload']['witness']==manifest('replay')['payload']['witness']
assert manifest('prefix')['payload']['input']['actions']==[]
assert manifest('prefix')['payload']==manifest('prefix-copy')['payload']
a=json.loads((root/'rooted/report.json').read_text())
b=json.loads((root/'continued/report.json').read_text())
assert b['executions_completed']==a['executions_completed']+4
for name in ('rooted','continued'):
    assert manifest(name)['payload']['input']['actions'][:1]==manifest('nonempty-prefix')['payload']['input']['actions']

assert json.loads((root/'timeline.json').read_text())==[{'step':0,'action':None}]
PYTHON
cp -R "$evidence/prefix" "$evidence/tampered"
printf corruption >> "$evidence/tampered/artifacts/input"
status=0
"$binary" replay "$evidence/tampered" --out "$evidence/refused" > "$evidence/tamper.txt" 2>&1 || status=$?
test "$status" -eq 2
test ! -e "$evidence/refused"
grep -q 'recorded artifact input has changed' "$evidence/tamper.txt"

python3 - "$binary" "$rom" "$recipe" "$evidence" <<'PYTHON'
import json, os, signal, subprocess, sys, time
from pathlib import Path
binary, rom, recipe, evidence = sys.argv[1:]
root = Path(evidence)
run = root / 'interrupted'
with (root / 'interrupted.log').open('w') as log:
    process = subprocess.Popen([binary, 'search', rom, '--config-toml', recipe,
        '--executions', '10000', '--out', str(run)], stdout=log, stderr=log, start_new_session=True)
    try:
        deadline = time.monotonic() + 45
        journal = run / 'package/checkpoints/checkpoints.jsonl'
        while True:
            if journal.exists():
                try:
                    records = [json.loads(line) for line in journal.read_text().splitlines()]
                    if records and (journal.parent / Path(records[-1]['file']).name).is_file():
                        break
                except (ValueError, KeyError):
                    pass
            if process.poll() is not None:
                raise AssertionError('search exited before interruption')
            if time.monotonic() >= deadline:
                raise AssertionError('no checkpoint within 45 seconds')
            time.sleep(0.05)
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
        process.wait()
records = []
for line in journal.read_text().splitlines():
    try:
        records.append(json.loads(line))
    except ValueError:
        break
completed = records[-1]['executions']
subprocess.run([binary, 'resume', str(run), '--executions', '4', '--out', str(root / 'recovered')], check=True)
report = json.loads((root / 'recovered/report.json').read_text())
assert report['executions_completed'] == completed + 4
PYTHON
