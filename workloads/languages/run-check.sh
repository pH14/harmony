#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: run-check.sh image evidence-directory}
evidence=${2:?usage: run-check.sh image evidence-directory}
binary=${HARMONY_BINARY:-target/release/harmony}
seed=${HARMONY_LANGUAGE_SEED:-17}
ram=${HARMONY_LANGUAGE_RAM_MIB:-1024}
mkdir -p "$evidence"
"$binary" preflight --image "$image" --json > "$evidence/preflight.json"
for boot in 1 2; do
    "$binary" oci run "$image" --seed "$seed" --ram-mib "$ram" --timeout 120 --out "$evidence/boot-$boot"
done
python3 "$(dirname "$0")/verify-runs.py" "$evidence/boot-1" "$evidence/boot-2" \
    --output "$evidence/determinism.json"
"$binary" oci run "$image" --seed "$seed" --ram-mib "$ram" --timeout 120 --out "$evidence/park" \
    -- /opt/harmony/park-launcher /opt/harmony/fixture
python3 - "$evidence/park" <<'PY'
import json
import sys
from pathlib import Path
directory = Path(sys.argv[1])
log = (directory / "serial.log").read_text()
assert json.loads((directory / "run.json").read_text())["container_rc"] == 0
assert log.count("HARMONY_LANGUAGE_PARK_PROGRESS\n") == 1
assert log.count("HARMONY_LANGUAGE_MARKER ") == 20
PY

if [[ $image == *language-java* ]]; then
    for boot in 1 2; do
        "$binary" oci run "$image" --seed "$seed" --ram-mib "$ram" --timeout 300 --out "$evidence/xcomp-$boot" \
            -- /opt/java/bin/java -Xcomp -cp /opt/harmony/java Fixture
    done
    python3 "$(dirname "$0")/verify-runs.py" "$evidence/xcomp-1" "$evidence/xcomp-2" \
        --output "$evidence/xcomp-determinism.json"
fi

if [[ $image == *language-c* ]]; then
    "$binary" oci run "$image" --seed "$seed" --ram-mib "$ram" --timeout 120 --out "$evidence/processes" -- /opt/harmony/fixture processes
    "$binary" oci run "$image" --seed "$seed" --ram-mib "$ram" --timeout 120 --out "$evidence/gcc" -- /opt/harmony/gcc-fixture
    python3 - "$evidence" <<'PY'
import json
import sys
from pathlib import Path
expected = [f"HARMONY_LANGUAGE_MARKER {marker:02}" for marker in range(1, 21)]
for name in ("processes", "gcc"):
    run = Path(sys.argv[1]) / name
    assert json.loads((run / "run.json").read_text())["container_rc"] == 0
    assert [line for line in (run / "serial.log").read_text().splitlines() if line.startswith("HARMONY_LANGUAGE_MARKER ")] == expected
PY
fi
