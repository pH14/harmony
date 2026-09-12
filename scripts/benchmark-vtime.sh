#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Re-run the pinned N3 PostgreSQL scenario on the M1 Max benchmark host.

set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <Image-postgres> <initramfs-postgres.cpio.gz> <new-report>" >&2
    exit 2
fi

image=$1
initramfs=$2
report=$3
pinned_image_sha256=08cafe8a473b56f7ad9274641cb661770bb45e11245fb504254ea6a154a499b1
pinned_initramfs_sha256=a7ec0987ff422f4c587f2d2ef54df194ae6de937420902319e9cb519c868905b

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "benchmark-vtime requires Apple Silicon macOS" >&2
    exit 2
fi
if [[ -e $report ]]; then
    echo "refusing to overwrite report: $report" >&2
    exit 2
fi

actual_image=$(shasum -a 256 "$image" | awk '{print $1}')
actual_initramfs=$(shasum -a 256 "$initramfs" | awk '{print $1}')
if [[ $actual_image != "$pinned_image_sha256" ]]; then
    echo "Image SHA-256 mismatch: $actual_image" >&2
    exit 1
fi
if [[ $actual_initramfs != "$pinned_initramfs_sha256" ]]; then
    echo "initramfs SHA-256 mismatch: $actual_initramfs" >&2
    exit 1
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
cargo build --release --locked --manifest-path workloads/tools/Cargo.toml --bin hvf_postgres
codesign --force --sign - \
    --entitlements consonance/vmm-backend/hvf.entitlements.plist \
    workloads/tools/target/release/hvf_postgres
workloads/tools/target/release/hvf_postgres "$image" "$initramfs" - "$report"

rg -q '^status PASS$' "$report"
rg -q '^checkpoint_hashes count=700 workers=8 status=PASS$' "$report"
rg -q '^exit_count_comparator event_loop=180012 raw_trace=180012 portable_trace=179271 substrate_private=741 status=PASS$' "$report"
rg -q '^trace events=179271 raw=180012 schedules=1139 digest=[0-9a-f]{64}$' "$report"

performance=$(rg '^performance_intrinsic status=PASS ' "$report")
wall_ns=$(sed -E 's/.*total_wall_ns=([0-9]+).*/\1/' <<<"$performance")
milli_eps=$(sed -E 's/.*milli_exits_per_second=([0-9]+).*/\1/' <<<"$performance")
echo "VTIME_M3_BENCHMARK status=PASS wall_ns=$wall_ns milli_exits_per_second=$milli_eps report=$report"
