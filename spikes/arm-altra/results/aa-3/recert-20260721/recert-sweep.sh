#!/usr/bin/env bash
# Continuous overnight AA-3 DIAGNOSTIC sweep (task 137, foreman directive 2026-07-22).
# Loops the 76-wide co-tenant campaign + quiet pinned-solo lane + full-join comparator +
# aggregate floor-check, FRESH seed base per cycle. Same fresh-pin DIAGNOSTIC basis as the
# recert cycle -- NOT an AA-3 GO certification; GO stays PARKED. On ANY solo!=cotenant
# divergence / overshoot / non-det PMI / floor FAIL: STOP immediately, preserve the diverging
# cycle's records, drop a P0 marker. Foreman stops gracefully via ~/aa3-sweep-STOP.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"
cd ~/aa3-recert/spikes/arm-altra
SPIKE=./target/release/arm-spike
FC=./target/release/floor-check
PHK=/home/ubuntu/kernel/linux-6.18.35-aa3preempt/vmlinux
NSHARD=76; FIRST_CORE=4; CASES=950
BANNER="DIAGNOSTIC mechanism re-verification at scale, fresh-pin basis — NOT an AA-3 GO certification (GO PARKED pending Paul's pin-basis ruling)"
common="--payload-dir payloads/target/aarch64-unknown-none/release \
  --payload-pins results/aa-3/recert-inputs/payload-pins.json \
  --host-kernel-image $PHK \
  --host-kernel-sha256 8e451458beb4a58475c82be816e66de4e1ab66ac2f852cd2314536b125282a3f \
  --host-kernel-build-id 899b921efe13f49eedff20784c0d61946880f9f7 \
  --environment results/aa-3/recert-inputs/environment.json \
  --weights results/aa-1b/weights-provisional.json"
rm -f "$HOME/aa3-sweep-P0" "$HOME/aa3-sweep-STOP"
RESLOG="$HOME/aa3-sweep-results.log"
echo "=== SWEEP START $(date -u +%FT%TZ) :: $BANNER ==="
cycle=1
while [ ! -f "$HOME/aa3-sweep-STOP" ]; do
  NN=$(printf "%02d" "$cycle")
  CBASE=$((3330000000000000 + cycle*100))   # cycle1=..0100, cycle2=..0200, non-overlapping (76 seeds/cycle, gap 24)
  DIR=results/aa-3/sweep-$NN
  OUT=$DIR/exact
  SOLO=$DIR/solo-ref
  echo "=== CYCLE $NN START $(date -u +%FT%TZ) seed_base=$CBASE :: $BANNER ==="
  sudo -n rm -rf "$DIR"; mkdir -p "$OUT"
  fail=0

  # PHASE A: 76-wide co-tenant (cores 4-79 concurrent = the determinism stress test; saturates first)
  pids=()
  for k in $(seq 0 $((NSHARD-1))); do
    core=$((FIRST_CORE + k)); seed=$((CBASE + k))
    sudo -n taskset -c "$core" $SPIKE run $common --core "$core" --stage aa3 --mechanism patched --with-targets \
      --exclude-payload wfi-idle --skid-margin 53 --scale 1e6 --cases $CASES --reps 2 \
      --condition co-tenant-other-core --seed "$seed" \
      --run-set-id "aa3-sweep-$NN-s$k" --out "$OUT/aa3-sweep-$NN-s$k" </dev/null &
    pids+=($!)
  done
  for p in "${pids[@]}"; do wait "$p" || fail=1; done

  # PHASE B: quiet pinned-solo reference, box now idle (core 4, seed == s0's CBASE)
  sudo -n taskset -c 4 $SPIKE run $common --core 4 --stage aa3 --mechanism patched --with-targets \
    --exclude-payload wfi-idle --skid-margin 53 --scale 1e6 --cases $CASES --reps 2 \
    --condition pinned-solo --seed "$CBASE" \
    --run-set-id "aa3-sweep-$NN-solo" --out "$SOLO" </dev/null || fail=1

  # PHASE 2: verify — full-join comparator (solo vs co-tenant s0) + aggregate floor-check
  sudo -n chown -R ubuntu:ubuntu "$DIR" 2>/dev/null || true
  python3 host/aa3-determinism-compare.py "$SOLO" "$OUT/aa3-sweep-$NN-s0" \
    > "$DIR/determinism.json" 2> "$DIR/determinism-transcript.txt"
  cmp_rc=$?
  dirs=""; for k in $(seq 0 $((NSHARD-1))); do dirs="$dirs $OUT/aa3-sweep-$NN-s$k"; done
  $FC $dirs --min-armed-overflows 1000000 --min-cases 500000 --min-reps 2 > "$DIR/floor-check-verdict.txt" 2>&1
  fc_rc=$?

  if [ "$fail" = 0 ] && [ "$cmp_rc" = 0 ] && [ "$fc_rc" = 0 ]; then
    line="cycle $NN seed_base=$CBASE $(date -u +%FT%TZ) RESULT=DIAGNOSTIC_GREEN solo==cotenant_MATCH floor_PASS (NOT a GO cert; GO PARKED)"
    echo "$line" | tee -a "$RESLOG"
    find "$DIR" -name records.jsonl -delete 2>/dev/null   # raw not preserved on GREEN (manifests+verdicts kept); disk hygiene
    touch "$HOME/aa3-sweep-cycle-$NN-DONE"
  else
    line="cycle $NN seed_base=$CBASE $(date -u +%FT%TZ) RESULT=P0_OR_FAIL fail=$fail cmp_rc=$cmp_rc fc_rc=$fc_rc — STOP+ESCALATE, records PRESERVED"
    echo "$line" | tee -a "$RESLOG"
    { echo "$line"; echo "diverging cycle dir (records preserved): $DIR"; grep -E 'solo_only_examples|cotenant_only_examples|multiplicity_mismatch|verdict|divergences' "$DIR/determinism.json" 2>/dev/null | head; grep -E '\[FAIL\]' "$DIR/floor-check-verdict.txt" 2>/dev/null | head; } > "$HOME/aa3-sweep-P0"
    break
  fi
  cycle=$((cycle+1))
done
echo "=== SWEEP END $(date -u +%FT%TZ) (cycles_completed=$((cycle-1)); STOP=$([ -f "$HOME/aa3-sweep-STOP" ] && echo yes || echo no); P0=$([ -f "$HOME/aa3-sweep-P0" ] && echo yes || echo no)) ==="
