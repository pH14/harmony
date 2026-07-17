#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-1(c) full parallel campaign orchestration (Paul's 2026-07-17 parallel directive).
# Phase 1 (QUIET, core 60): the cross-check SOLO reference + the pinned-solo real-scale grid.
# Phase 2 (host/aa1c-parallel.sh): the three co-tenant conditions sharded across cores 4-79
# concurrently + the co-tenant cross-check shard + the migration probe.
#
# The pre-existing quiet pinned-solo SMOKE bulk (250k, from the earlier serial lane at
# results/aa-1c/aa1c-pinned-solo-r2-bulk) is the pinned-solo condition's volume; this adds only
# its real-scale grid cells. Phase 1 runs while the box is idle so both the solo reference and
# the pinned-solo grid are genuinely uncontended.
#
# Detached:
#   nohup setsid bash host/aa1c-run-all.sh <tag> <bulk_cases> <grid_cases> </dev/null \
#       >~/aa1c-all-<tag>.log 2>&1 &
# Success marker: ~/aa1c-all-<tag>-OK — written ONLY if every phase/shard exited 0.
set -uo pipefail

TAG="${1:?usage: aa1c-run-all.sh <tag> <bulk_cases> [grid_cases]}"
BULK="${2:-420}"
GRID="${3:-6}"
cd ~/harmony/spikes/arm-altra

SPIKE=./target/release/arm-spike
ENV=results/aa-1b/inputs/environment.json
PINS=results/aa-1b/inputs/payload-pins.json
WEIGHTS=results/aa-1b/weights-provisional.json
HK=/home/ubuntu/kernel/linux-6.18.35/vmlinux
HKSHA=9bd4870d878de39f29009072f7d2a099528882fae1f5bd34c6304079bfec7890
HKBID=1e975db8ae7fa463a78c6190c4079a88409ab888
PDIR=payloads/target/aarch64-unknown-none/release
OUT=results/aa-1c/parallel
mkdir -p "$OUT"
common="--payload-dir $PDIR --payload-pins $PINS \
  --host-kernel-image $HK --host-kernel-sha256 $HKSHA --host-kernel-build-id $HKBID \
  --stage aa1 --mechanism stock --with-targets --environment $ENV --weights $WEIGHTS"

fail=0

# ---- Phase 1: QUIET (core 60, box idle) ----
# Cross-check SOLO reference: seed 9000000000000001, cases 400 — MUST match the co-tenant
# xcheck shard in aa1c-parallel.sh (same seed AND cases), so all 8 payloads' tuples align.
echo "== quiet: cross-check solo reference =="
taskset -c 60 $SPIKE run $common --core 60 --scale smoke --cases 400 --reps 1 \
  --condition pinned-solo --seed 9000000000000001 \
  --run-set-id "aa1c-xref-$TAG" --out "$OUT/aa1c-xref-$TAG" </dev/null || fail=1
# Pinned-solo real-scale grid: gives the pinned-solo condition its 1e6/1e7/1e8 cells (the
# normative grid-cell check needs them) + a solo real-scale skid reference.
echo "== quiet: pinned-solo grid =="
taskset -c 60 $SPIKE run $common --core 60 --scale 1e6 --scale 1e7 --scale 1e8 --cases 6 --reps 1 \
  --condition pinned-solo --seed 1000000000000777 \
  --run-set-id "aa1c-pinned-solo-$TAG-grid" --out "$OUT/aa1c-pinned-solo-$TAG-grid" </dev/null || fail=1

if [ "$fail" != "0" ]; then echo "AA1C_QUIET_PHASE_FAILED"; exit 1; fi

# ---- Phase 2: PARALLEL co-tenant (cores 4-79 concurrent) ----
bash host/aa1c-parallel.sh "$TAG" "$BULK" "$GRID" || fail=1

if [ "$fail" = "0" ]; then
  touch ~/aa1c-all-"$TAG"-OK
  echo "AA1C_RUN_ALL_OK"
else
  echo "AA1C_RUN_ALL_FAILED — inspect the run-sets; do NOT declare GO"
  exit 1
fi
