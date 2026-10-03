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
"$binary" init --language c --image harmony-cli-investigation:local
python3 - "$repo" <<'PY'
import json
import platform
import sys
from pathlib import Path
root = Path(sys.argv[1]) / "consonance/harmony-linux/build" / platform.machine()
kernel = root / ("Image" if platform.machine() == "aarch64" else "bzImage")
config = Path("harmony.toml")
config.write_text(f"kernel = {json.dumps(str(kernel))}\n"
                  f"base_initramfs = {json.dumps(str(root / 'initramfs-oci.cpio.gz'))}\n"
                  "wall_seconds = 30\n" + config.read_text() +
                  "\n[hooks]\ndebug = ['/opt/harmony/application', 'debug']\n")
PY
"$binary" prepare --json > prepared.json
python3 -c 'import json; json.load(open("prepared.json"))'
"$binary" doctor --offline --json > doctor.json
"$binary" run --name baseline
"$binary" branch baseline --at-step 0 --inject 'hook debug 1s' --follow --name verbose
"$binary" logs verbose --contains 'debug logging' > debug.log
grep -q '^application debug logging enabled$' debug.log
"$binary" replay verbose --name verified
"$binary" branch baseline --at-step 0 --name prefix
"$binary" replay prefix --name prefix-copy
"$binary" replay prefix-copy --name prefix-copy-copy
"$binary" inspect verbose --json > inspected.json
"$binary" timeline verbose --json > timeline.json
python3 - <<'PY'
import json
steps = json.load(open("timeline.json"))
assert any(s['observation']['hooks_started'] > 0 for s in steps)
assert any(s['observation']['hooks_finished'] > 0 for s in steps)
assert any('application debug logging enabled' in s['console'] for s in steps)
PY
status=0
"$binary" search --from prefix --executions 4 --for 30s --name neighborhood || status=$?
test "$status" -le 1
status=0
"$binary" search --resume neighborhood --executions 8 --for 30s --name extended || status=$?
test "$status" -le 1
python3 - <<'PY'
import json
from pathlib import Path
runs = Path('.harmony/runs')
a = json.loads((runs / 'neighborhood/report.json').read_text())
b = json.loads((runs / 'extended/report.json').read_text())
assert b['executions'] > a['executions']
PY
cp -R .harmony/runs/prefix .harmony/runs/tampered
printf corruption >> .harmony/runs/tampered/artifacts/kernel
status=0
"$binary" replay tampered --name refused > tamper.txt 2>&1 || status=$?
test "$status" -eq 2
grep -q 'recorded artifact kernel has changed' tamper.txt
test ! -e .harmony/runs/refused
