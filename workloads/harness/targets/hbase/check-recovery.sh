#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
image=${1:?usage: check-recovery.sh image evidence-directory [config.toml] [actions.json]}
evidence=${2:?evidence-directory required}
recipe=$(cd "$(dirname "$0")" && pwd)
config=${3:-$recipe/harmony.toml}
actions=${4:-$recipe/recovery.json}
binary=${HARMONY_BINARY:-target/release/harmony}
"$binary" prepare "$image"
"$binary" debug run "$image" --config "$config" --seed 17 --for 20m \
 --actions "$actions" --out "$evidence/run"
python3 "$recipe/check-recovery.py" "$evidence/run/report.json" "$actions"
"$binary" debug replay "$evidence/run" --out "$evidence/replay"
