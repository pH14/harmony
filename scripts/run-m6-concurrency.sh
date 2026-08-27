#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
run_dir=$(mktemp -d "${TMPDIR:-/tmp}/harmony-m6.XXXXXX")
report=${1:-$run_dir/m6-report.json}

env CARGO_TARGET_DIR="$run_dir/rust-target" cargo build \
    --manifest-path "$repo_root/harmony-linux/concurrency-suite/rust-lost-update/Cargo.toml" \
    --release
(
    cd "$repo_root/harmony-linux/concurrency-suite/go-publish-before-init"
    env GOCACHE="$run_dir/go-cache" go build -trimpath \
        -o "$run_dir/m6-go-publish-before-init" .
)
cargo build --manifest-path "$repo_root/dissonance/Cargo.toml" \
    --release --bin m6-concurrency

"$repo_root/dissonance/target/release/m6-concurrency" \
    "$run_dir/rust-target/release/m6-rust-lost-update" \
    "$run_dir/m6-go-publish-before-init" \
    "$report"

"$repo_root/scripts/m6-independent-oracle.py" \
    "$report" \
    "$repo_root/harmony-linux/concurrency-suite/m6-plan.json" \
    "$repo_root/dissonance/searcher/src/bin/m6-concurrency.rs"

if "$repo_root/scripts/m6-independent-oracle.py" \
    "$report" \
    "$repo_root/harmony-linux/concurrency-suite/m6-plan.json" \
    "$repo_root/dissonance/searcher/src/bin/m6-concurrency.rs" \
    --plant-schedule go_publish_before_init; then
    echo "M6 planted comparator negative unexpectedly passed" >&2
    exit 1
fi
echo "M6_INDEPENDENT_NEGATIVE_OK id=go_publish_before_init field=reproducer_schedule"
if "$repo_root/scripts/m6-independent-oracle.py" \
    "$report" \
    "$repo_root/harmony-linux/concurrency-suite/m6-plan.json" \
    "$repo_root/dissonance/searcher/src/bin/m6-concurrency.rs" \
    --plant-held-out-fixture; then
    echo "M6 planted held-out fixture unexpectedly passed" >&2
    exit 1
fi
echo "M6_FIXTURE_NEGATIVE_OK id=go_publish_before_init field=held_out_schedule"
echo "M6_REPORT=$report"
echo "M6_RUN_DIR=$run_dir"
