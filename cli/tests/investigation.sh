#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
binary=${HARMONY_BINARY:-$repo/target/release/harmony}
evidence=${1:?usage: investigation.sh EVIDENCE_DIRECTORY}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
export HARMONY_SDK_DIR="$repo"
cp "$repo/cli/tests/fixtures/logging.c" "$evidence/main.c"
cd "$evidence"
"$binary" init harmony-cli-investigation:local --language c
python3 - "$repo" <<'PY'
import json
import platform
import sys
from pathlib import Path
root = Path(sys.argv[1]) / "consonance/harmony-linux/build" / platform.machine()
kernel = root / ("Image" if platform.machine() == "aarch64" else "bzImage")
config = Path("harmony.toml")
text = config.read_text().replace('[runner.options]', '[runner.options]\n' +
    f"kernel = {json.dumps(str(kernel))}\nbase_initramfs = {json.dumps(str(root / 'initramfs-oci.cpio.gz'))}")
text = text.replace('[search]', '[search]\nwall_seconds = 30')
config.write_text(text)
PY
"$binary" prepare --json > prepared.json
python3 -c 'import json; json.load(open("prepared.json"))'
"$binary" check --offline --json > check.json
"$binary" search --executions 1 --name baseline
"$binary" branch baseline --step 0 --exec 'test ! -e /tmp/debug; touch /tmp/debug; echo COMMAND_SAVED' --stop --name verbose
"$binary" branch verbose --exec 'test -f /tmp/debug && echo RESTORED_COMMAND' --stop --name verified
"$binary" show verified --logs --contains RESTORED_COMMAND > debug.log
grep -q '^RESTORED_COMMAND' debug.log
printf 'test -f /tmp/debug && touch /tmp/local-script-saved && echo LOCAL_SCRIPT\n' > debug.sh
"$binary" branch verbose --exec-file ./debug.sh --stop --name scripted
cmp debug.sh .harmony/runs/scripted/artifacts/debug-script.sh
rm debug.sh
printf 'test -t 0 && test -f /tmp/local-script-saved && touch /tmp/shell-saved && echo GUEST_PTY\nexit\n' | \
    "$binary" branch scripted --shell --name interactive
"$binary" branch interactive --exec 'test -f /tmp/shell-saved && echo RESTORED_SHELL' --stop --name shell-copy
"$binary" show shell-copy --logs --json > shell-logs.json
python3 - <<'PYTHON'
import json
assert any('RESTORED_SHELL' in line for section in json.load(open('shell-logs.json')) for line in section['lines'])
PYTHON
"$binary" branch baseline --step 0 --stop --name prefix
"$binary" branch prefix --stop --name prefix-copy
"$binary" show verbose --json > inspected.json
"$binary" show verbose --timeline --json > timeline.json
python3 - <<'PYTHON'
import json
assert json.load(open('inspected.json'))['manifest']['mode'] == 'branch'
assert json.load(open('timeline.json'))[0]['step'] == 0
PYTHON
status=0
"$binary" search --from interactive --executions 2 --for 30s --name shell-search || status=$?
test "$status" -le 1
"$binary" branch shell-search --step 0 --exec 'test -f /tmp/shell-saved && echo SEARCH_RESTORED_SHELL' --stop --name shell-search-root
"$binary" show shell-search-root --logs --contains SEARCH_RESTORED_SHELL > search-root.log
grep -q '^SEARCH_RESTORED_SHELL' search-root.log
status=0
"$binary" search --from prefix --executions 4 --for 30s --name neighborhood || status=$?
test "$status" -le 1
status=0
"$binary" search --resume neighborhood --seed 9223372036854775808 --name invalid-seed > invalid-seed.txt 2>&1 || status=$?
test "$status" -eq 2
grep -q 'maximum integer' invalid-seed.txt
test ! -e .harmony/runs/invalid-seed
printf '{"file":' >> .harmony/runs/neighborhood/checkpoints/checkpoints.jsonl
status=0
"$binary" search --resume neighborhood --executions 4 --for 30s --name extended || status=$?
test "$status" -le 1
python3 - <<'PY'
import json
from pathlib import Path
runs = Path('.harmony/runs')
a = json.loads((runs / 'neighborhood/report.json').read_text())
b = json.loads((runs / 'extended/report.json').read_text())
assert b['executions'] == a['executions'] + 4
assert json.loads((runs / 'prefix/manifest.json').read_text())['payload']['settle'] is False
for name in ('neighborhood', 'extended'):
    assert json.loads((runs / name / 'manifest.json').read_text())['payload']['settle'] is True
PY
cp -R .harmony/runs/prefix .harmony/runs/tampered
printf corruption >> .harmony/runs/tampered/artifacts/kernel
status=0
"$binary" branch tampered --stop --name refused > tamper.txt 2>&1 || status=$?
test "$status" -eq 2
grep -q 'recorded artifact kernel has changed' tamper.txt
test ! -e .harmony/runs/refused
