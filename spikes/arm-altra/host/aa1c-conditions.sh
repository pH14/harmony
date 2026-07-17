#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-1(c) armed-overflow condition matrix (docs/ARM-ALTRA.md §AA-1(c): count
# exactness + PMI multiplicity + skid, under the four contamination conditions).
# The guest-mode analogue of host/el0-conditions.sh. One run-set per
# (condition × phase); floor-check sums them for the cumulative ≥10^6 armed floor.
#
# Mechanism: `stock` (a host-side signal kicks the vCPU out of KVM_RUN) — the
# pre-patch overflow path AA-3 later moves in-kernel. Every armed overflow must be
# delivered exactly once; skid (landed-target) is MEASURED here, not bounded.
#
# Conditions (same labels the checker's REQUIRED_AA1_CONDITIONS demands):
#   pinned-solo           — quiet box
#   co-tenant-other-core  — busy spinners on the load cores
#   co-tenant-same-core   — one busy spinner on the measurement core itself
#   memory-pressure       — stress-ng VM workers on the load cores
# The claim under test: wall clock may move under load; BR_RETIRED counts may not.
#
# Phases per condition:
#   bulk  — scale 1e6 (grid-valid AND cheapest): 8 payloads x $BULK armed overflows.
#           This carries the condition's share of the cumulative floor.
#   grid  — scales 1e7 + 1e8: the differential the per-class density table needs, and
#           the grid presence the checker requires at every (payload x condition x scale).
# Plus, once, unpinned: the bounded migration probe (rr #3607 missed-PMI mode).
#
# Distinct master seed per condition: CaseKey = (payload, scale, seed, target) has NO
# condition field, so identical seeds would collapse the union's distinct-case count.
#
# Run detached, state-based wait:
#   nohup setsid bash host/aa1c-conditions.sh <tag> <bulk_cases> [grid_cases] \
#       </dev/null >~/aa1c-<tag>.log 2>&1 &
# Success marker: ~/aa1c-<tag>-OK — written ONLY if every stage exited 0 (RC-propagated).
set -euo pipefail

TAG="${1:?usage: aa1c-conditions.sh <tag> <bulk_cases_per_payload> [grid_cases]}"
BULK="${2:?bulk cases per payload per condition (8*BULK = armed overflows/condition at 1e6)}"
GRID="${3:-30}"

CORE=60                                   # measurement core (cores 0-3 housekeeping)
LOAD_CORES=(61 62 63 64 65 66 67 68 69)   # co-tenant / memory-pressure load
cd ~/harmony/spikes/arm-altra

SPIKE=./target/release/arm-spike
ENV=results/aa-1b/inputs/environment.json
PINS=results/aa-1b/inputs/payload-pins.json
WEIGHTS=results/aa-1b/weights-provisional.json
HK=/home/ubuntu/kernel/linux-6.18.35/vmlinux
HKSHA=9bd4870d878de39f29009072f7d2a099528882fae1f5bd34c6304079bfec7890
HKBID=1e975db8ae7fa463a78c6190c4079a88409ab888
PDIR=payloads/target/aarch64-unknown-none/release

common="--payload-dir $PDIR --payload-pins $PINS \
  --host-kernel-image $HK --host-kernel-sha256 $HKSHA --host-kernel-build-id $HKBID \
  --stage aa1 --mechanism stock --with-targets --environment $ENV --weights $WEIGHTS"

declare -A SEED=(
  [pinned-solo]=1111111111111111
  [co-tenant-other-core]=2222222222222222
  [co-tenant-same-core]=3333333333333333
  [memory-pressure]=4444444444444444
)

spin_pids=()
cleanup() { for p in "${spin_pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; spin_pids=(); }
trap cleanup EXIT

run_condition() {
  local cond="$1"; local seed="${SEED[$cond]}"
  $SPIKE run $common --core $CORE --scale 1e6 --cases "$BULK" --reps 1 \
    --condition "$cond" --seed "$seed" \
    --run-set-id "aa1c-$cond-$TAG-bulk" --out "results/aa-1c/aa1c-$cond-$TAG-bulk"
  $SPIKE run $common --core $CORE --scale 1e7 --scale 1e8 --cases "$GRID" --reps 1 \
    --condition "$cond" --seed "$seed" \
    --run-set-id "aa1c-$cond-$TAG-grid" --out "results/aa-1c/aa1c-$cond-$TAG-grid"
}

# Smoke-fire the exact configuration once before the spend (§Smoke once before spend).
echo "== pre-spend smoke =="
$SPIKE run $common --core $CORE --scale 1e6 --cases 1 --reps 1 \
  --condition pinned-solo --seed 999 \
  --run-set-id "aa1c-presmoke-$TAG" --out "results/aa-1c/aa1c-presmoke-$TAG"

echo "== pinned-solo =="
run_condition pinned-solo

echo "== co-tenant-other-core =="
for c in "${LOAD_CORES[@]}"; do taskset -c "$c" bash -c 'while :; do :; done' & spin_pids+=($!); done
run_condition co-tenant-other-core
cleanup

echo "== memory-pressure =="
taskset -c 61-69 stress-ng --vm 8 --vm-bytes 2G --quiet & spin_pids+=($!)
sleep 3
run_condition memory-pressure
cleanup

# same-core contention halves the measurement core's throughput, so its bulk takes
# ~2x wall time — run it last so the faster conditions land their checkpoints first.
echo "== co-tenant-same-core =="
taskset -c $CORE bash -c 'while :; do :; done' & spin_pids+=($!)
run_condition co-tenant-same-core
cleanup

# The bounded migration probe: ONE unpinned run, the vCPU thread rotated across the
# cpuset while overflows are armed (rr #3607). Needs >=2 allowed cores; taskset opens
# the whole measurement+load set. Re-pin is automatic — every other run above pins.
echo "== migration probe (bounded, unpinned) =="
taskset -c 60-69 $SPIKE run $common --migration-probe --scale 1e6 --cases 200 --reps 1 \
  --condition migration-probe --seed 5555555555555555 \
  --run-set-id "aa1c-migration-$TAG" --out "results/aa-1c/aa1c-migration-$TAG"

touch ~/aa1c-"$TAG"-OK
echo "AA1C_ALL_OK"
