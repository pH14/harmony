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
expected_image=91b4f5781c32b01e9d10a7762f7a8951e83d49a9442edd72e8f61f8dc10a72f0
expected_initramfs=c3939c777730b95335c1e518c6d09225eba00898a841164b28822ea19a1b66ab
expected_trace=8418141803debd1e19a1e4e8cb47b77787a73be29b76497af66a69d8572c5ab8

if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo "benchmark-vtime-m3 requires Apple Silicon macOS" >&2
    exit 2
fi
if [[ -e $report ]]; then
    echo "refusing to overwrite report: $report" >&2
    exit 2
fi

actual_image=$(shasum -a 256 "$image" | awk '{print $1}')
actual_initramfs=$(shasum -a 256 "$initramfs" | awk '{print $1}')
if [[ $actual_image != "$expected_image" ]]; then
    echo "Image SHA-256 mismatch: $actual_image" >&2
    exit 1
fi
if [[ $actual_initramfs != "$expected_initramfs" ]]; then
    echo "initramfs SHA-256 mismatch: $actual_initramfs" >&2
    exit 1
fi

repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
cargo build --release --locked -p vmm-core --bin hvf_postgres_m3
codesign --force --sign - \
    --entitlements consonance/vmm-backend/hvf.entitlements.plist \
    target/release/hvf_postgres_m3
target/release/hvf_postgres_m3 "$image" "$initramfs" - "$report"

rg -q '^status PASS$' "$report"
rg -q '^checkpoint_hashes count=149 workers=8 status=PASS$' "$report"
rg -q '^exit_count_comparator event_loop=41278 raw_trace=41278 portable_trace=38295 substrate_private=2983 status=PASS$' "$report"
rg -q "^trace events=38295 raw=41278 schedules=10451 digest=$expected_trace$" "$report"

performance=$(rg '^performance_intrinsic status=PASS ' "$report")
wall_ns=$(sed -E 's/.*total_wall_ns=([0-9]+).*/\1/' <<<"$performance")
milli_eps=$(sed -E 's/.*milli_exits_per_second=([0-9]+).*/\1/' <<<"$performance")
echo "VTIME_M3_BENCHMARK status=PASS wall_ns=$wall_ns milli_exits_per_second=$milli_eps report=$report"
