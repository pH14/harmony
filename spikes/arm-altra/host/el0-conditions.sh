#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-1(a) evidence run: the EL0 condition matrix (docs/ARM-ALTRA.md §AA-1
# contamination probes, EL0 half). One run-set per condition; el0-check sums them.
#
# Conditions (same labels as the guest AA-1 matrix):
#   pinned-solo           — quiet box
#   co-tenant-other-core  — 8 busy spinners pinned to cores 62-69
#   co-tenant-same-core   — 1 busy spinner pinned to the measurement core itself
#   memory-pressure       — stress-ng VM workers on cores 62-69
#
# The claim under test: wall clock may move, counts may not.
#
# Run detached: nohup bash el0-conditions.sh <run-tag> > ~/el0-cond.log 2>&1 &
# Success marker: ~/el0-cond-OK (state-based wait; RC-propagated: written only
# if EVERY stage exited 0).
set -euo pipefail

TAG="${1:?usage: el0-conditions.sh <run-tag>}"
CORE=61
LOAD_CORES=(62 63 64 65 66 67 68 69)
cd ~/harmony/spikes/arm-altra
EL0=./target/release/arm-el0-count
ENV=results/aa-1a/inputs/environment.json
SCALES=(--scale 1e6 --scale 1e7 --scale 1e8)
DIMS=(--cases 3 --reps 10)

spin_pids=()
cleanup() {
  for p in "${spin_pids[@]:-}"; do kill "$p" 2>/dev/null || true; done
  spin_pids=()
}
trap cleanup EXIT

run_one() {
  local cond="$1"
  # Smoke-fire the exact condition once (1 case, 1 rep, smoke scale) before spend.
  $EL0 --core $CORE --cases 1 --reps 1 --condition "$cond" \
    --environment $ENV --run-set-id "aa1a-$cond-$TAG-smoke" \
    --out "results/aa-1a/aa1a-$cond-$TAG-smoke"
  # The measured set.
  $EL0 --core $CORE "${SCALES[@]}" "${DIMS[@]}" --condition "$cond" \
    --environment $ENV --run-set-id "aa1a-$cond-$TAG" \
    --out "results/aa-1a/aa1a-$cond-$TAG"
}

echo "== pinned-solo =="
run_one pinned-solo

echo "== co-tenant-other-core =="
for c in "${LOAD_CORES[@]}"; do
  taskset -c "$c" bash -c 'while :; do :; done' &
  spin_pids+=($!)
done
run_one co-tenant-other-core
cleanup

echo "== co-tenant-same-core =="
taskset -c $CORE bash -c 'while :; do :; done' &
spin_pids+=($!)
run_one co-tenant-same-core
cleanup

echo "== memory-pressure =="
taskset -c 62-69 stress-ng --vm 8 --vm-bytes 2G --quiet &
spin_pids+=($!)
sleep 3
run_one memory-pressure
cleanup

# Grade the matrix BEFORE the success marker: the marker attests a PASSING el0-check
# verdict, not merely that the runs completed. el0-check recomputes every floor from the
# retained records (aggregation + one-attested-tool, the 5×4 class×condition matrix, the
# 1e6/1e7/1e8 scale sweep, oracle-exactness); its transcript is itself retained evidence
# (docs/ARM-ALTRA.md §Evidence-integrity #2 — "the disposition may not be written until the
# checker passes; the checker's output is itself retained evidence").
echo "== grade =="
CHECK=./target/release/el0-check
VERDICT="results/aa-1a/el0-verdict-$TAG.txt"
if $CHECK \
  "results/aa-1a/aa1a-pinned-solo-$TAG" \
  "results/aa-1a/aa1a-co-tenant-other-core-$TAG" \
  "results/aa-1a/aa1a-co-tenant-same-core-$TAG" \
  "results/aa-1a/aa1a-memory-pressure-$TAG" \
  --min-reps 10 --min-cases 3 | tee "$VERDICT"; then
  touch ~/el0-cond-OK
  echo "EL0_CONDITIONS_OK"
else
  echo "EL0_CONDITIONS_FAILED — el0-check rejected the matrix; see $VERDICT (no marker written)"
  exit 1
fi
