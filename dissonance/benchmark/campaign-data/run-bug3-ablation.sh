#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task 69 M2 — PR#90 ABLATION (Paul-authorized): bug-3 SIGNAL config only, 20 seeds,
# --explore-period 1 (explore every branch, NO exploit — a RECORDED flag, PR#90
# round-2, not an env). Same seeds/budget/deadline/calibration/image as the main
# campaign. Separates "cells blind" from "exploit budget harmful on rare-value bugs":
# at ep=1 the signal draws the identical PRNG stream as baseline, so if its find-rate
# matches baseline's 18/20 (not the ep=4 campaign's 11/20), the EXPLOIT is what hurt.
# 3-wide via box-window leases; detached. Propagates rc (PR#90 round-2 blocking-2):
# any nonzero campaign rc (incl. the zero-cell HARD-FAIL) marks the phase FAILED and
# the orchestrator exits nonzero.
# Launch: setsid nohup bash run-bug3-ablation.sh >~/t69m2-results/bug3-abl-orch.out 2>&1 </dev/null &
set -uo pipefail
cd ~/harmony-t69m2 && source ~/.cargo/env
BIN=./target/release/conductor
CAL=dissonance/benchmark/campaign-data/bug1/calibration.json
IMG=initramfs-uuid.cpio.gz; RMARK=UUID_READY
OUT=~/t69m2-results/bug3-ablation
mkdir -p "$OUT" "$OUT/traces"
for lf in failures finds determinism; do : > "$OUT/$lf.log"; done  # PR#90 final: truncate per-run so a stale FIND/hash from a same-OUT rerun can't mask a mismatch
DEADLINE=50000; MAXB=512; RN=25; EP=1
PROG="$OUT/progress.log"
echo "$(date +%FT%T) ABLATION START (signal explore-period=$EP) deadline=$DEADLINE maxb=$MAXB rn=$RN" >> "$PROG"

run_one() {  # core name seed
  local core=$1 name=$2 seed=$3
  echo "$(date +%T) START $name core=$core" >> "$PROG"
  taskset -c "$core" $BIN bench-campaign --bug 3 --config signal --seed "$seed" \
    --max-branches $MAXB --deadline-delta $DEADLINE --replay-n $RN --explore-period $EP \
    --calibration "$CAL" --initramfs "$IMG" --ready-marker "$RMARK" \
    --out "$OUT/$name.json" --record "$OUT/traces/$name.traces.json" \
    </dev/null >"$OUT/$name.log" 2>&1
  local rc=$?
  grep '^\[conductor\] FIND' "$OUT/$name.log" | sed "s|^|$name |" >> "$OUT/finds.log"
  echo "$(date +%T) DONE $name rc=$rc $(grep -o '[0-9]* certified find(s)' "$OUT/$name.log"|tail -1) $(grep -o '[0-9]* distinct signal cells' "$OUT/$name.log"|tail -1)" >> "$PROG"
  if [ "$rc" -ne 0 ]; then echo "$name rc=$rc" >> "$OUT/failures.log"; echo "$(date +%T) FAILED $name rc=$rc" >> "$PROG"; fi
  return $rc
}

~/box-window.sh acquire a1 >"$OUT/.c1" 2>>"$OUT/win.log"; c1=$(cat "$OUT/.c1")
~/box-window.sh acquire a2 >"$OUT/.c2" 2>>"$OUT/win.log"; c2=$(cat "$OUT/.c2")
~/box-window.sh acquire a3 >"$OUT/.c3" 2>>"$OUT/win.log"; c3=$(cat "$OUT/.c3")
echo "$(date +%T) leased cores: [$c1] [$c2] [$c3]" >> "$PROG"
if [ -z "$c1" ] || [ -z "$c2" ] || [ -z "$c3" ]; then
  echo "$(date +%T) FATAL: could not lease 3 cores" >> "$PROG"
  for w in a1 a2 a3; do ~/box-window.sh release $w 2>>"$OUT/win.log"; done; exit 1
fi
jobs=(); for s in $(seq 1 20); do jobs+=("$s"); done
stream() { local core=$1 start=$2 i; for ((i=start; i<${#jobs[@]}; i+=3)); do run_one "$core" "b3-signal-ep1-${jobs[$i]}" "${jobs[$i]}" || true; done; }
stream "$c1" 0 & stream "$c2" 1 & stream "$c3" 2 & wait
for w in a1 a2 a3; do ~/box-window.sh release $w 2>>"$OUT/win.log"; done
echo "$(date +%FT%T) PHASE1 done" >> "$PROG"

for seed in 1 2; do
  ~/box-window.sh acquire abl-solo-$seed --exclusive >"$OUT/.csolo" 2>>"$OUT/win.log"; core=$(cat "$OUT/.csolo")
  run_one "$core" "b3-signal-ep1-$seed-solo" "$seed" || true
  ~/box-window.sh release abl-solo-$seed 2>>"$OUT/win.log"
done
echo "$(date +%FT%T) PHASE2 solo done" >> "$PROG"

{
  echo "=== ablation determinism spot-check (signal ep=1, co-tenant vs solo) ==="
  for seed in 1 2; do
    co=$(grep "^b3-signal-ep1-$seed " "$OUT/finds.log" | grep -o 'state_hash [0-9a-f]*' | head -1)
    so=$(grep "^b3-signal-ep1-$seed-solo " "$OUT/finds.log" | grep -o 'state_hash [0-9a-f]*' | head -1)
    if [ -z "$co" ] && [ -z "$so" ]; then echo "seed $seed AGREE (no find in either — non-event)"
    elif [ -n "$co" ] && [ "$co" = "$so" ]; then echo "seed $seed OK $co"
    else echo "seed $seed P0-DIVERGENCE co=[$co] solo=[$so]"; fi
  done
} >> "$OUT/determinism.log"

nf=$(wc -l < "$OUT/failures.log")
if [ "$nf" -gt 0 ]; then
  echo "$(date +%FT%T) ABLATION FAILED — $nf campaign(s) rc!=0 (see failures.log)" >> "$PROG"; exit 1
fi
echo "$(date +%FT%T) ABLATION DONE (0 failures)" >> "$PROG"
