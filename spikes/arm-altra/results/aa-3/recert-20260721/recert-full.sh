#!/usr/bin/env bash
# AA-3 re-cert EXTENDED DIAGNOSTIC (task 137, hm-idb). Faithful to the certified
# host/aa3-exact-shard.sh semantics (48d519f): 76 co-tenant shards cores 4-79,
# --scale 1e6 --cases 950 --reps 2, seeds 3330000000000001+k, + a quiet pinned-solo
# reference lane sharing s0's seed, + the full-join determinism comparator + the
# aggregate floor-check at the normative floors. Adapted ONLY for the post-wipe box
# reality: current kernel attestation pins, box-local regenerated payload pins (fresh
# build of the byte-identical certified source), on-silicon environment, per-invocation
# sudo for /dev/kvm. Harness logic / comparators / acceptance criteria: UNCHANGED.
#
# LABEL: DIAGNOSTIC mechanism re-verification at scale, fresh-pin basis — NOT an AA-3 GO
# certification. The GO disposition stays PARKED pending Paul's pin-basis ruling.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd ~/aa3-recert/spikes/arm-altra
SPIKE=./target/release/arm-spike
FC=./target/release/floor-check
PHK=/home/ubuntu/kernel/linux-6.18.35-aa3preempt/vmlinux
TAG=recert; NSHARD=76; FIRST_CORE=4; CASES=950
DONE="$HOME/aa3-recert-full-DONE"; rm -f "$DONE"
BANNER="DIAGNOSTIC mechanism re-verification at scale, fresh-pin basis — NOT an AA-3 GO certification (GO PARKED pending Paul's pin-basis ruling)"
echo "=== START $(date -u +%FT%TZ) :: $BANNER ==="

common="--payload-dir payloads/target/aarch64-unknown-none/release \
  --payload-pins results/aa-3/recert-inputs/payload-pins.json \
  --host-kernel-image $PHK \
  --host-kernel-sha256 8e451458beb4a58475c82be816e66de4e1ab66ac2f852cd2314536b125282a3f \
  --host-kernel-build-id 899b921efe13f49eedff20784c0d61946880f9f7 \
  --environment results/aa-3/recert-inputs/environment.json \
  --weights results/aa-1b/weights-provisional.json"

SOLO=results/aa-3/exact-solo-ref
OUT=results/aa-3/exact-recert
sudo -n rm -rf "$SOLO" "$OUT"
mkdir -p "$OUT"
fail=0

echo "=== PHASE 0 $(date -u +%FT%TZ): quiet SOLO reference (core 4, seed 3330000000000001, pinned-solo) ==="
sudo -n taskset -c 4 $SPIKE run $common --core 4 --stage aa3 --mechanism patched --with-targets \
  --exclude-payload wfi-idle --skid-margin 53 --scale 1e6 --cases $CASES --reps 2 \
  --condition pinned-solo --seed 3330000000000001 \
  --run-set-id aa3-exact-solo-ref --out "$SOLO" </dev/null || fail=1
echo "PHASE0_done $(date -u +%FT%TZ) fail=$fail"

echo "=== PHASE 1 $(date -u +%FT%TZ): 76-wide CO-TENANT campaign (cores 4-79 concurrent = the determinism stress test) ==="
pids=()
for k in $(seq 0 $((NSHARD-1))); do
  core=$((FIRST_CORE + k)); seed=$((3330000000000000 + k + 1))
  sudo -n taskset -c "$core" $SPIKE run $common --core "$core" --stage aa3 --mechanism patched --with-targets \
    --exclude-payload wfi-idle --skid-margin 53 --scale 1e6 --cases $CASES --reps 2 \
    --condition co-tenant-other-core --seed "$seed" \
    --run-set-id "aa3-exact-$TAG-s$k" --out "$OUT/aa3-exact-$TAG-s$k" </dev/null &
  pids+=($!)
done
for p in "${pids[@]}"; do wait "$p" || fail=1; done
echo "PHASE1_done $(date -u +%FT%TZ) fail=$fail"

echo "=== PHASE 2 $(date -u +%FT%TZ): verify — full-join determinism comparator + aggregate floor-check ==="
CMP="$HOME/recert-full-determinism.txt"
python3 host/aa3-determinism-compare.py "$SOLO" "$OUT/aa3-exact-$TAG-s0" > "$CMP" 2>&1
cmp_rc=$?; echo "COMPARATOR_rc=$cmp_rc"; tail -25 "$CMP"

FCOUT="$HOME/recert-full-floorcheck.txt"
shard_dirs=""
for k in $(seq 0 $((NSHARD-1))); do shard_dirs="$shard_dirs $OUT/aa3-exact-$TAG-s$k"; done
$FC $shard_dirs --min-armed-overflows 1000000 --min-cases 500000 --min-reps 2 > "$FCOUT" 2>&1
fc_rc=$?; echo "FLOORCHECK_rc=$fc_rc"; tail -4 "$FCOUT"

sudo -n chown -R ubuntu:ubuntu "$SOLO" "$OUT" 2>/dev/null || true

if [ "$fail" = 0 ] && [ "$cmp_rc" = 0 ] && [ "$fc_rc" = 0 ]; then
  echo "RESULT=DIAGNOSTIC_ALL_GREEN shards_ok solo==cotenant_MATCH aggregate_floor_PASS -- NOT a GO cert (GO PARKED)" | tee "$DONE"
else
  echo "RESULT=NEEDS_REVIEW fail=$fail cmp_rc=$cmp_rc fc_rc=$fc_rc -- P0 if overshoot/non-det-PMI/solo!=cotenant; PARK+escalate" | tee "$DONE"
fi
echo "=== END $(date -u +%FT%TZ) :: $BANNER ==="
