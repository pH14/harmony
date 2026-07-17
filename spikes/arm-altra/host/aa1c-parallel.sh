#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-1(c) PARALLEL co-tenant campaign (Paul's 2026-07-17 directive). Altra has no SMT, so
# "pinned-solo" is one workload PER PHYSICAL CORE, not one-core-total: sharding the matrix
# across the idle cores (each tuple on its OWN dedicated core, concurrently) both collapses
# the serial run to minutes AND *is* the co-tenant determinism stress test — with the counts
# frequency-independent (V-time, AA-1(b)), solo == co-tenant digests MUST hold.
#
# This runs the three CO-TENANT conditions AFTER the quiet pinned-solo reference (the serial
# aa1c-conditions.sh pinned-solo lane) has completed. Each condition's shards run CONCURRENTLY
# across cores 4..(4+NSHARD-1) (cores 0-3 = housekeeping). Every shard is `taskset`-pinned to
# its own core with per-run posture attestation; count-exactness vs the analytical oracle on
# every co-tenant record is the co-tenant count-determinism check (any count != oracle is a P0
# determinism finding). A dedicated cross-check shard reuses the pinned-solo seed so
# host/aa1c-determinism-check.py can compare solo vs co-tenant state_digests per tuple.
#
# P0 DISCIPLINE: a count mismatch or a solo!=cotenant digest is a determinism finding — STOP
# and report, NEVER serialize to make it disappear. This script only GATHERS; the floor-check
# + determinism-check grade.
#
# Run detached:
#   nohup setsid bash host/aa1c-parallel.sh <tag> <bulk_cases> <grid_cases> </dev/null \
#       >~/aa1c-par-<tag>.log 2>&1 &
# Success marker: ~/aa1c-par-<tag>-OK — written ONLY if every shard of every phase exited 0.
set -uo pipefail   # NOT -e: shard RCs are collected explicitly across concurrent waits.

TAG="${1:?usage: aa1c-parallel.sh <tag> <bulk_cases_per_shard> [grid_cases_per_shard]}"
BULK="${2:?bulk cases/payload/shard at SMOKE (8*BULK*NSHARD = armed/condition)}"
GRID="${3:-6}"     # grid cases/payload/scale/shard at 1e6,1e7,1e8

FIRST_CORE=4
NSHARD=76          # cores 4..79
GRID_SHARDS=8      # grid (1e8 is slow) sharded across 8 cores: 4..11
XREF=9000000000000001    # dedicated cross-check seed. A matching SOLO reference must be run
# separately (quiet): `arm-spike run --core 60 --seed 9000000000000001 --scale smoke
# --cases 400 --condition pinned-solo --run-set-id aa1c-xref-<tag>`. Using the SAME seed AND
# the SAME --cases as the co-tenant xcheck below makes the plans identical, so all 8 payloads'
# tuples align (a different case-count would shift the per-cell RNG stream and align only
# payload 0). host/aa1c-determinism-check.py compares xref (solo) vs xcheck (co-tenant) FINAL
# state_digests per tuple. BOTH are EXCLUDED from the floor aggregate: they share
# (payload,scale,seed), and armed replay-identity compares the skid-dependent landed_digest,
# which legitimately differs solo-vs-cotenant, so aggregating them would false-fail it.
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

# Distinct seed base per condition; each shard adds its index, so no two shards share tuples.
declare -A CSEED=(
  [co-tenant-other-core]=6000000000000000
  [memory-pressure]=7000000000000000
  [co-tenant-same-core]=8000000000000000)

load_pids=()
kill_load() { for p in "${load_pids[@]:-}"; do kill "$p" 2>/dev/null || true; done; load_pids=(); }
trap kill_load EXIT

# Launch a sharded phase: $1 cond, $2 base_seed, $3 scale-args, $4 cases, $5 nshard,
# $6 tag-suffix, $7 same_core(1|0). Waits for every shard; returns nonzero if any failed.
run_sharded() {
  local cond="$1" base="$2" scaleargs="$3" cases="$4" nsh="$5" sfx="$6" samecore="${7:-0}"
  local pids=() fail=0 k core seed sc_pid
  for k in $(seq 0 $((nsh-1))); do
    core=$((FIRST_CORE + k))
    seed=$((base + k + 1))
    if [ "$samecore" = "1" ]; then
      # co-tenant-same-core: a busy spinner shares THIS shard's physical core.
      taskset -c "$core" bash -c 'while :; do :; done' </dev/null &
      load_pids+=($!)
    fi
    taskset -c "$core" $SPIKE run $common --core "$core" $scaleargs --cases "$cases" --reps 1 \
      --condition "$cond" --seed "$seed" \
      --run-set-id "aa1c-$cond-$TAG-$sfx$k" --out "$OUT/aa1c-$cond-$TAG-$sfx$k" </dev/null &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p" || fail=1; done
  [ "$samecore" = "1" ] && kill_load
  return $fail
}

fail=0

# ---- co-tenant-other-core: 76 concurrent shards ARE each other's other-core co-tenants ----
echo "== co-tenant-other-core (bulk, $NSHARD shards) =="
run_sharded co-tenant-other-core "${CSEED[co-tenant-other-core]}" "--scale smoke" "$BULK" "$NSHARD" b || fail=1
echo "== co-tenant-other-core (grid, $GRID_SHARDS shards) =="
run_sharded co-tenant-other-core "$(( ${CSEED[co-tenant-other-core]} + 500 ))" "--scale 1e6 --scale 1e7 --scale 1e8" "$GRID" "$GRID_SHARDS" g || fail=1
# determinism cross-check: pinned-solo tuples RE-RUN under co-tenancy (one core, others idle now
# but this rides while the box is warm; the compare is solo-vs-this). Small subset of the
# pinned-solo plan (same seed, fewer cases => a prefix of the same tuples).
echo "== determinism cross-check (pinned-solo seed under co-tenant load) =="
for k in $(seq 0 15); do taskset -c $((FIRST_CORE+k)) bash -c 'while :; do :; done' </dev/null & load_pids+=($!); done
taskset -c $((FIRST_CORE+20)) $SPIKE run $common --core $((FIRST_CORE+20)) --scale smoke --cases 400 --reps 1 \
  --condition co-tenant-other-core --seed "$XREF" \
  --run-set-id "aa1c-xcheck-$TAG" --out "$OUT/aa1c-xcheck-$TAG" </dev/null || fail=1
kill_load

# ---- memory-pressure: measurement shards + stress-ng VM workers competing for memory BW ----
echo "== memory-pressure (bulk) =="
taskset -c 4-79 stress-ng --vm 16 --vm-bytes 2G --quiet </dev/null & load_pids+=($!)
sleep 3
run_sharded memory-pressure "${CSEED[memory-pressure]}" "--scale smoke" "$BULK" "$NSHARD" b || fail=1
run_sharded memory-pressure "$(( ${CSEED[memory-pressure]} + 500 ))" "--scale 1e6 --scale 1e7 --scale 1e8" "$GRID" "$GRID_SHARDS" g || fail=1
kill_load

# ---- co-tenant-same-core: every shard shares its physical core with a busy spinner ----
echo "== co-tenant-same-core (bulk) =="
run_sharded co-tenant-same-core "${CSEED[co-tenant-same-core]}" "--scale smoke" "$BULK" "$NSHARD" b 1 || fail=1
run_sharded co-tenant-same-core "$(( ${CSEED[co-tenant-same-core]} + 500 ))" "--scale 1e6 --scale 1e7 --scale 1e8" "$GRID" "$GRID_SHARDS" g 1 || fail=1

# ---- migration probe: bounded, unpinned, once (rr #3607) ----
echo "== migration probe =="
taskset -c 4-79 $SPIKE run $common --core 4 --migration-probe --scale 1e6 --cases 400 --reps 1 \
  --condition migration-probe --seed 5000000000000001 \
  --run-set-id "aa1c-migration-$TAG" --out "$OUT/aa1c-migration-$TAG" </dev/null || fail=1

if [ "$fail" = "0" ]; then
  touch ~/aa1c-par-"$TAG"-OK
  echo "AA1C_PARALLEL_ALL_OK"
else
  echo "AA1C_PARALLEL_FAILED (a shard exited nonzero) — inspect the run-sets; do NOT declare GO"
  exit 1
fi
