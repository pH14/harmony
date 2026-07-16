#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task 69 M2 bug-3 campaign orchestrator (runs ON THE BOX, detached via setsid).
# 20 seeds x 2 configs, 3-wide, small deadline + marker-based certification
# (foreman-approved). Then 3 solo --exclusive determinism spot-checks: co-tenant
# vs solo state_hash MUST match (a mismatch is a P0 leak, per the M2 directives).
#
# ROBUST 3-WIDE DESIGN (two earlier bugs taught this): DO NOT background
# `box-window.sh acquire` per campaign — concurrent first-acquires race the
# window-open (load_patched) and all but one ABORT with an empty core. Instead:
# acquire 3 PERSISTENT leases SERIALLY up front (opens the window once, cleanly),
# run the 40 jobs as 3 fixed-core SERIAL streams, release the 3 leases at the end
# (the last release reverts to stock). Solo spot-checks run AFTER that, so their
# --exclusive acquire sees a reverted window and opens cleanly.
#
# Launch:  setsid nohup bash run-bug3-campaign.sh >~/t69m2-results/bug3-orch.out 2>&1 </dev/null &
# Watch:   tail -f ~/t69m2-results/bug3/progress.log     (and ~/t69m2-results/bug3/determinism.log)
# Results: ~/t69m2-results/bug3/*.json  (+ finds.log with per-find state_hash)
set -uo pipefail
cd ~/harmony-t69m2 && source ~/.cargo/env
BIN=./target/release/campaign-runner
CAL=dissonance/benchmark/campaign-data/bug1/calibration.json
IMG=initramfs-uuid.cpio.gz
RMARK=UUID_READY
OUT=~/t69m2-results/bug3
mkdir -p "$OUT" "$OUT/traces"   # traces/ = the retained re-key substrate (SCORING R1/R2)
for lf in failures finds determinism; do : > "$OUT/$lf.log"; done  # PR#90 final: truncate per-run so a stale FIND/hash from a same-OUT rerun can't mask a mismatch (the solo compare greps head -1)
DEADLINE=50000; MAXB=512; RN=25
PROG="$OUT/progress.log"
echo "$(date +%FT%T) ORCH START deadline=$DEADLINE maxb=$MAXB rn=$RN" >> "$PROG"

run_campaign() {  # core name config seed
  local core=$1 name=$2 config=$3 seed=$4
  echo "$(date +%T) START $name core=$core" >> "$PROG"
  taskset -c "$core" $BIN bench-campaign --bug 3 --config "$config" --seed "$seed" \
    --max-branches $MAXB --deadline-delta $DEADLINE --replay-n $RN \
    --calibration "$CAL" --initramfs "$IMG" --ready-marker "$RMARK" \
    --out "$OUT/$name.json" --record "$OUT/traces/$name.traces.json" \
    </dev/null >"$OUT/$name.log" 2>&1
  local rc=$?
  grep '^\[campaign-runner\] FIND' "$OUT/$name.log" | sed "s|^|$name |" >> "$OUT/finds.log"
  echo "$(date +%T) DONE $name rc=$rc $(grep -o '[0-9]* certified find(s)' "$OUT/$name.log"|tail -1) $(grep -o '[0-9]* distinct signal cells' "$OUT/$name.log"|tail -1)" >> "$PROG"
  if [ "$rc" -ne 0 ]; then echo "$name rc=$rc" >> "$OUT/failures.log"; echo "$(date +%T) FAILED $name rc=$rc" >> "$PROG"; fi
  return $rc
}

# --- Phase 1: 3 persistent leases (serial), 3 fixed-core serial streams -----
# CRITICAL: acquire with stdout redirected to a FILE, never `core=$(...acquire...)`.
# box-window.sh records $PPID as the lease's liveness pid; under command
# substitution $PPID is the transient `$(...)` subshell, which dies instantly, so
# the NEXT acquire's sweep_stale reaps the lease, sees 0 leases, re-runs
# load_patched, and ABORTs (already patched) -> empty core. A redirected simple
# command keeps box-window.sh's parent = this long-lived orchestrator.
~/box-window.sh acquire w1 >"$OUT/.c1" 2>>"$OUT/win.log"; c1=$(cat "$OUT/.c1")
~/box-window.sh acquire w2 >"$OUT/.c2" 2>>"$OUT/win.log"; c2=$(cat "$OUT/.c2")
~/box-window.sh acquire w3 >"$OUT/.c3" 2>>"$OUT/win.log"; c3=$(cat "$OUT/.c3")
echo "$(date +%T) leased cores: [$c1] [$c2] [$c3]" >> "$PROG"
if [ -z "$c1" ] || [ -z "$c2" ] || [ -z "$c3" ]; then
  echo "$(date +%T) FATAL: could not lease 3 cores" >> "$PROG"
  for w in w1 w2 w3; do ~/box-window.sh release $w 2>>"$OUT/win.log"; done
  exit 1
fi

jobs=(); for s in $(seq 1 20); do jobs+=("baseline:$s" "signal:$s"); done
stream() {  # core start-index
  local core=$1 start=$2 i
  for ((i=start; i<${#jobs[@]}; i+=3)); do
    IFS=: read -r cfg seed <<< "${jobs[$i]}"
    run_campaign "$core" "b3-$cfg-$seed" "$cfg" "$seed" || true  # rc tracked in failures.log; keep the stream going
  done
}
stream "$c1" 0 &
stream "$c2" 1 &
stream "$c3" 2 &
wait
for w in w1 w2 w3; do ~/box-window.sh release $w 2>>"$OUT/win.log"; done
echo "$(date +%FT%T) PHASE1 done" >> "$PROG"

# --- Phase 2: solo determinism spot-checks (baseline seeds 1..3, exclusive) -
for seed in 1 2 3; do
  ~/box-window.sh acquire solo-$seed --exclusive >"$OUT/.csolo" 2>>"$OUT/win.log"
  core=$(cat "$OUT/.csolo")
  run_campaign "$core" "b3-baseline-$seed-solo" baseline "$seed"
  ~/box-window.sh release solo-$seed 2>>"$OUT/win.log"
done
echo "$(date +%FT%T) PHASE2 solo done" >> "$PROG"

# --- Phase 3: compare co-tenant vs solo state_hash (P0 on divergence) -------
{
  echo "=== determinism spot-check (co-tenant vs solo) ==="
  for seed in 1 2 3; do
    co=$(grep "^b3-baseline-$seed " "$OUT/finds.log" | grep -o 'state_hash [0-9a-f]*' | head -1)
    so=$(grep "^b3-baseline-$seed-solo " "$OUT/finds.log" | grep -o 'state_hash [0-9a-f]*' | head -1)
    if [ -z "$co" ] && [ -z "$so" ]; then
      echo "seed $seed AGREE (no certified find in EITHER config — a non-event; empty==empty is agreement, NOT a divergence)"
    elif [ -n "$co" ] && [ "$co" = "$so" ]; then
      echo "seed $seed OK $co"
    else
      echo "seed $seed P0-DIVERGENCE co=[$co] solo=[$so]"
    fi
  done
} >> "$OUT/determinism.log"
# rc propagation (PR#90 round-2): a swallowed campaign failure (zero-cell HARD-FAIL,
# KVM failure, transport death) must fail the wrapper, not exit 0.
nf=$(wc -l < "$OUT/failures.log")
if [ "$nf" -gt 0 ]; then
  echo "$(date +%FT%T) ORCH FAILED — $nf campaign(s) rc!=0 (see failures.log)" >> "$PROG"; exit 1
fi
echo "$(date +%FT%T) ORCH DONE (0 failures)" >> "$PROG"
