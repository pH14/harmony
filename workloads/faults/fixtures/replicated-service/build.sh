#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the fixture commands for the selected guest architecture.
set -eu

target=${TARGET:-x86_64-unknown-linux-musl}
out=${OUT:-dist}
cargo build --release --target "$target" --offline
mkdir -p "$out"
for binary in fault-replica fault-check fault-recovery fault-pending fault-ready; do
    cp "target/$target/release/$binary" "$out/$binary"
done
cp workload.json network-init.sh netns-exec "$out/"
