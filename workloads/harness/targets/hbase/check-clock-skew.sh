#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: check-clock-skew.sh image evidence-directory [config.toml]}
evidence=${2:?evidence-directory required}
recipe=$(cd "$(dirname "$0")" && pwd)
config=${3:-$recipe/harmony.toml}
binary=${HARMONY_BINARY:-target/release/harmony}
"$binary" prepare "$image"
set +e
"$binary" debug run "$image" --config "$config" --seed 17 --for 30m \
 --actions "$recipe/clock-skew.json" --out "$evidence/run"
run_status=$?
set -e
test "$run_status" -eq 1
python3 "$recipe/check-clock-skew.py" "$evidence/run/report.json" "$recipe/clock-skew.json"
"$binary" debug replay "$evidence/run" --out "$evidence/replay"
