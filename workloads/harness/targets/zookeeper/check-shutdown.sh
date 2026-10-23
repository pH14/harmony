#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: check-shutdown.sh image evidence-directory [config.toml]}
evidence=${2:?evidence-directory required}
binary=${HARMONY_BINARY:-target/release/harmony}
config=()
if [ "$#" -ge 3 ]; then config=(--config "$3"); fi
mkdir -p "$evidence"
"$binary" prepare "$image" --json > "$evidence/admission.json"
for seed in 17 18; do
 "$binary" debug run "$image" "${config[@]}" --seed "$seed" --for 600s \
  --out "$evidence/seed-$seed" -- /opt/harmony/measure.sh
 python3 - "$evidence/seed-$seed" <<'PY'
import json
from pathlib import Path
import sys
root = Path(sys.argv[1])
record = json.loads((root / 'run.json').read_text())
assert record['container_rc'] == 0, record
log = (root / 'serial.log').read_text()
assert log.count('JVM_READY ') == 1
assert 'HARMONY_OCI_EXIT rc=0' in log
PY
done
"$binary" debug replay "$evidence/seed-17" --out "$evidence/replay"
