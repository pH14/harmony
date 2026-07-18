#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# AA-3 exact-landing run (sharded, patched mechanism): >=10^6 armed deadlines that land
# work == target EXACTLY (run_until_overflow + single_step), across cores 4-79 concurrently
# (Paul's parallel ruling; per-core is the intended posture on this no-SMT box). Each shard is
# taskset-pinned to its own core with per-run posture attestation; RC-propagated over every
# shard. --skid-margin 53 (the AA-1 measured margin) arms the overflow below target so the
# Preempt fires reliably (no arm-at-target boundary loss) and single-step walks up to target.
# reps 2 so replay-identity grades the LANDED digests (the exact landing makes them
# deterministic). wfi-idle is count-exempt at AA-3 (foreman ruling — AA-5-domain timer finding).
#
# Detached: nohup setsid bash host/aa3-exact-shard.sh <tag> <cases_per_shard> </dev/null \
#     >~/aa3-exact-<tag>.log 2>&1 &
# Success marker: ~/aa3-exact-<tag>-OK (written only if every shard exits 0 AND the retained
# quiet solo reference fully joins and MATCHes co-tenant shard s0).
set -uo pipefail

TAG="${1:?usage: aa3-exact-shard.sh <tag> <cases_per_shard>}"
OK_MARKER="${HOME}/aa3-exact-${TAG}-OK"
# Invalidate any prior same-tag success before the first shard can fail.
rm -f -- "$OK_MARKER"
CASES="${2:?cases/payload/shard (8*CASES*2reps*NSHARD = armed; keep CASES<=20000 for 1e6 branch-dense)}"
NSHARD=76
FIRST_CORE=4
cd ~/harmony/spikes/arm-altra
SPIKE=./target/release/arm-spike
PHK=/home/ubuntu/kernel/linux-6.18.35-patched/vmlinux
common="--payload-dir payloads/target/aarch64-unknown-none/release --payload-pins results/aa-1b/inputs/payload-pins.json \
  --host-kernel-image $PHK --host-kernel-sha256 65a5fa6f7c6a55005c6523b595ff725a86508aa874f05dcf86368309fd68fcff \
  --host-kernel-build-id df0f4f02bd425383bb312faf8ccb94a67352216d \
  --environment results/aa-3/inputs/environment.json --weights results/aa-1b/weights-provisional.json"
OUT=results/aa-3/exact; rm -rf "$OUT"; mkdir -p "$OUT"

pids=(); fail=0
for k in $(seq 0 $((NSHARD-1))); do
  core=$((FIRST_CORE + k)); seed=$((3330000000000000 + k + 1))
  taskset -c "$core" $SPIKE run $common --core "$core" --stage aa3 --mechanism patched --with-targets --exclude-payload wfi-idle \
    --skid-margin 53 --scale 1e6 --cases "$CASES" --reps 2 --condition co-tenant-other-core --seed "$seed" \
    --run-set-id "aa3-exact-$TAG-s$k" --out "$OUT/aa3-exact-$TAG-s$k" </dev/null &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || fail=1; done

echo "== solo/co-tenant full-join determinism grade =="
DET_JSON="$OUT/determinism-$TAG.json"
DET_TRANSCRIPT="$OUT/determinism-$TAG.txt"
if [ "$fail" = 0 ]; then
  if python3 host/aa3-determinism-compare.py \
    results/aa-3/exact-solo-ref "$OUT/aa3-exact-$TAG-s0" \
    >"$DET_JSON" 2>"$DET_TRANSCRIPT"; then
    cat "$DET_TRANSCRIPT"
  else
    fail=1
    cat "$DET_TRANSCRIPT"
    echo "AA3_DETERMINISM_FAILED — no success marker; see $DET_JSON and $DET_TRANSCRIPT"
  fi
fi

if [ "$fail" = 0 ]; then
  touch "$OK_MARKER"
  echo "AA3_EXACT_ALL_OK (shards + full-join determinism MATCH)"
else
  echo "AA3_EXACT_FAILED (shard or comparator) — inspect the run-sets"
  exit 1
fi
