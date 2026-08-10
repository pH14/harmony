#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the aa4-mislabel-evasion forged ELF and verify the STATIC half of the
# anti-weakening proof: the section-aware scanner PASSES it (the exclusives are in
# a data-flagged section it excludes) even though the exclusive bytes are physically
# present for the runtime guard to reject. The runtime half runs on the aa4guard
# host via `arm-spike aa4-guard-reject`.
set -euo pipefail
cd "$(dirname "$0")"
SCANNER=../../host/aa4-exclusive-scan.py
OUT=aa4-mislabel-evasion.elf

echo "== assemble + link the forged ELF"
cc -nostdlib -static -Wl,-e,_start -Wl,-T,aa4-mislabel-evasion.ld -o "$OUT" aa4-mislabel-evasion.s
echo "ok: $OUT  sha256=$(sha256sum "$OUT" | cut -d' ' -f1)"

echo "== confirm .rodata is a NON-exec (data) section and carries the planted exclusives"
readelf -SW "$OUT" | grep -E "\.text|\.rodata" || true
# The exclusive words must physically exist in the file (so the runtime guard finds them).
if ! objdump -s -j .rodata "$OUT" | grep -qiE "207c5f88|207c0288"; then
    echo "FAIL: planted LDXR/STXR bytes (little-endian 885f7c20/88027c20) not found in .rodata" >&2
    exit 1
fi
echo "ok: planted exclusive bytes present in the data-flagged .rodata section"

echo "== STATIC section-aware scanner must PASS the forged ELF (the evasion under proof)"
if python3 "$SCANNER" "$OUT"; then
    echo "PASS(static-evasion): the section-aware scanner accepts the mislabeled ELF (exclusives hidden in .rodata)"
else
    echo "FAIL: the section-aware scanner rejected the fixture — it is NOT demonstrating the evasion" >&2
    exit 1
fi
echo ""
echo "NEXT: on the aa4guard host, prove the runtime guard REJECTS it:"
echo "  arm-spike aa4-guard-reject --image $OUT --image-sha256 <sha> --core <n>"
