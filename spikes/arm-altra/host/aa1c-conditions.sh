#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-1(c) armed-overflow condition matrix (docs/ARM-ALTRA.md §AA-1(c): count
# exactness + PMI multiplicity + skid, under the four contamination conditions).
# The guest-mode analogue of host/el0-conditions.sh. One run-set per
# (condition x phase); floor-check sums them for the cumulative >=10^6 armed floor.
#
# Mechanism: `stock` (a host-side signal kicks the vCPU out of KVM_RUN) — the
# pre-patch overflow path AA-3 later moves in-kernel. Every armed overflow must be
# delivered exactly once (per-record multiplicity); skid (landed-target) is MEASURED.
#
# TWO-SCALE DECOMPOSITION (why):
#   bulk  — scale SMOKE. Carries the delivery-reliability VOLUME: the >=10^6 armed
#           floor certifies that the arm->fire->signal-kick->land cycle delivers
#           exactly once and stays count-exact, which the cycle exercises identically
#           at any scale. Smoke is the cheapest scale that runs that cycle, and — key —
#           its branch-dense oracle is only 1000 trips/seed, so the floor-checker's
#           MAX_ORACLE_TRIPS ceiling (2e10) grades 31,250 distinct branch-dense seeds
#           trivially. (A 1e6 bulk at that seance count is 3.1e10 trips: ungradeable.)
#   grid  — scales 1e6 + 1e7 + 1e8. Carries the SCALE SCIENCE: the per-class density
#           table, count-exactness at scale, the grid-cell presence the checker
#           requires, and — the point — the skid_margin. Skid is scale-DEPENDENT
#           (branches retired during a fixed kick latency grow with the loop's branch
#           rate), so the worst-case margin lives at 1e8, not smoke. reps=1 everywhere:
#           an armed record's replay key is its landed_digest, which is skid-dependent,
#           so same-input reps would diverge by construction — reps are for counting
#           runs, not armed ones.
# Plus, once, unpinned: the bounded migration probe (rr #3607 missed-PMI mode), at 1e6
# so the armed interval is long enough for the churner to migrate the thread inside it.
#
# Distinct master seed per condition/phase: CaseKey = (payload, scale, seed, target)
# has NO condition field, so identical seeds would collapse the union's distinct-case
# count — every run below draws its own.
#
# Run detached, state-based wait:
#   nohup setsid bash host/aa1c-conditions.sh <tag> <bulk_cases> <grid_cases> <mig_cases> \
#       </dev/null >~/aa1c-<tag>.log 2>&1 &
# Success marker: ~/aa1c-<tag>-OK — written ONLY if every stage exited 0 (RC-propagated).
set -euo pipefail

TAG="${1:?usage: aa1c-conditions.sh <tag> <bulk_cases> <grid_cases> <mig_cases>}"
BULK="${2:?bulk cases/payload/condition at SMOKE (8*BULK = armed/condition; keep <=31250)}"
GRID="${3:-100}"   # grid cases/payload/scale at 1e6,1e7,1e8 (keep <=180: 180*1e8 < 2e10)
MIG="${4:-400}"    # migration-probe cases/payload at 1e6

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

# Distinct master seeds: one per (condition, phase) so no two runs share a case key.
declare -A BSEED=(
  [pinned-solo]=1000000000000001 [co-tenant-other-core]=2000000000000001
  [memory-pressure]=3000000000000001 [co-tenant-same-core]=4000000000000001)
declare -A GSEED=(
  [pinned-solo]=1000000000000777 [co-tenant-other-core]=2000000000000777
  [memory-pressure]=3000000000000777 [co-tenant-same-core]=4000000000000777)

spin_pids=()
cleanup() { for p in "${spin_pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; spin_pids=(); }
trap cleanup EXIT

run_condition() {
  local cond="$1"
  $SPIKE run $common --core $CORE --scale smoke --cases "$BULK" --reps 1 \
    --condition "$cond" --seed "${BSEED[$cond]}" \
    --run-set-id "aa1c-$cond-$TAG-bulk" --out "results/aa-1c/aa1c-$cond-$TAG-bulk"
  $SPIKE run $common --core $CORE --scale 1e6 --scale 1e7 --scale 1e8 --cases "$GRID" --reps 1 \
    --condition "$cond" --seed "${GSEED[$cond]}" \
    --run-set-id "aa1c-$cond-$TAG-grid" --out "results/aa-1c/aa1c-$cond-$TAG-grid"
}

# Smoke-fire the exact bulk+grid config once before the spend (§Smoke once before spend).
echo "== pre-spend smoke =="
$SPIKE run $common --core $CORE --scale smoke --cases 2 --reps 1 \
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
# the whole set. At 1e6 so the armed interval is long enough to span a migration.
echo "== migration probe (bounded, unpinned) =="
# --core is required by the CLI even here; in --migration-probe mode it is read but not
# used to pin (the churner rotates across the cpuset instead), so its value is inert.
taskset -c 60-69 $SPIKE run $common --core $CORE --migration-probe --scale 1e6 --cases "$MIG" --reps 1 \
  --condition migration-probe --seed 5000000000000001 \
  --run-set-id "aa1c-migration-$TAG" --out "results/aa-1c/aa1c-migration-$TAG"

touch ~/aa1c-"$TAG"-OK
echo "AA1C_ALL_OK"
