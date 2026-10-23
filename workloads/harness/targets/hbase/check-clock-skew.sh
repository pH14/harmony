#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: check-clock-skew.sh image evidence-directory [config.toml]}
evidence=${2:?evidence-directory required}
recipe=$(cd "$(dirname "$0")" && pwd)
config=${3:-$recipe/harmony.toml}
actions=$recipe/clock-skew.json
binary=${HARMONY_BINARY:-target/release/harmony}
"$binary" prepare "$image" --config "$config"
set +e
"$binary" debug run "$image" --config "$config" --seed 17 --for 30m \
 --actions "$actions" --out "$evidence/run"
run_status=$?
set -e
if [ "$run_status" -ne 1 ]; then
  printf 'expected a consistency finding (exit 1); Harmony exited %s\n' "$run_status" >&2
  exit 1
fi
python3 "$recipe/check-clock-skew.py" "$evidence/run/report.json" "$actions"
"$binary" debug replay "$evidence/run" --out "$evidence/replay"
python3 "$recipe/check-clock-skew.py" "$evidence/replay/report.json" "$actions"
