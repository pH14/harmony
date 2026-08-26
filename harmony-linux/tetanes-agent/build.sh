#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the M2 guest agent natively on the pinned Linux/aarch64 host, then
# fail closed if its complete linked image contains a live counter or LL/SC.
set -euo pipefail

cd "$(dirname "$0")"

if [ "$(uname -sm)" != "Linux aarch64" ]; then
    echo "FAIL: tetanes-agent guest build needs native Linux/aarch64 (validated on msr1)" >&2
    exit 1
fi

repo_root=$(cd ../.. && pwd)
export RUSTFLAGS="-C target-cpu=neoverse-n1 -C target-feature=+lse -C link-arg=-Wl,--build-id=none"
cargo build --locked --release --bin harmony-tetanes-agent
agent=$PWD/target/release/harmony-tetanes-agent

python3 "$repo_root/harmony-linux/scripts/aa4-exclusive-scan.py" "$agent"
python3 "$repo_root/harmony-linux/scripts/aa5-counter-scan.py" "$agent"
echo "$agent"
