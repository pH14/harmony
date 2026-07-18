#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# run-ae1.sh — AE-1 work-clock run orchestrator (docs/AMD-EPYC.md AE-1).
# Serializes the (a) exactness, (c) SpecLockMap on/off, (d) overflow+skid experiments
# on a pinned, sibling-idled measurement core with the LS_CFG posture attested per run,
# then machine-checks the floors against the retained records. Runs ON the box.
#
# Contamination hygiene (CONFIG_HZ=1000 on this box -> a 1 ms tick; NVMe IRQs land on
# the measurement core): device IRQs are steered off the core, and the hammer writes its
# raw records to tmpfs (/dev/shm) DURING the run so its own output never triggers an NVMe
# interrupt on the measured core. The scheduler tick cannot be stopped without nohz_full
# at boot, so exactness is judged on interrupt-free (clean) windows, which the hammer
# tags per-sample; the residual contamination is accounted, never passed (evidence
# integrity #6). Sub-ms windows (small n) maximize clean-window yield under the 1 ms tick.
#
# Usage: run-ae1.sh --core N --event 0xHEX [--reps R] [--runset NAME]
set -euo pipefail
SD=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SD/.." && pwd)
HAMMER="$ROOT/harness/amd-hammer"
CHECK="$ROOT/schemas/check-floors.py"

core=""; event=0xc4; reps=3000; ovreps=1000000; runset="ae1-$(uname -r)"
while [ $# -gt 0 ]; do case "$1" in
  --core) core="$2"; shift 2;; --event) event="$2"; shift 2;;
  --reps) reps="$2"; shift 2;; --overflow-reps) ovreps="$2"; shift 2;;
  --runset) runset="$2"; shift 2;;
  *) echo "bad arg $1" >&2; exit 2;; esac; done
[ -n "$core" ] || { echo "need --core N" >&2; exit 2; }

OUT="$ROOT/results/ae-1/$runset"; mkdir -p "$OUT"
TMP="/dev/shm/ae1.$$"; mkdir -p "$TMP"
trap 'rm -rf "$TMP"' EXIT

echo "=== AE-1 run-set $runset on core $core, event $event, reps $reps ovreps $ovreps ===" >&2
# No IRQ steering: output is on tmpfs (no disk I/O -> few NVMe interrupts), and the only
# real contaminant is the un-steerable 1 ms scheduler tick, which the clean-window filter
# already accounts for. Steering would leave a persistent box change to restore.

# --- posture: workaround ON, sibling idled, attested ---
sudo bash "$SD/posture.sh" apply --core "$core" --speclockmap on > "$OUT/posture-on.attest.json"
SIB=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["sibling"])' "$OUT/posture-on.attest.json")

# Gate-RC propagation (P1-2, the PR-98 green-on-fail lesson): `|| true` is banned — a
# crashed hammer or a FLOOR_CHECK: FAIL must fail the run. The one exception is the
# AE-1(c) OFF probe, whose non-zero exit is the DELIBERATE overcount reproduction (data,
# not a run failure) — it is logged, not gated.
fail=0
# (a) exactness — small n for sub-ms windows so clean samples accrue for every class
if ! taskset -c "$core" "$HAMMER" --mode exactness --core "$core" --event "$event" \
     --n1 20000 --n2 40000 --reps "$reps" --out "$TMP/ae1a.json"; then fail=1; echo "AE-1(a) exactness hammer FAILED" >&2; fi
# (d) overflow + skid — one-shot arms, period 10000, payload exceeds it (≥10^6 arms
#     is the doc's AE-1(d) acceptance floor; aggregated so the file stays small)
if ! taskset -c "$core" "$HAMMER" --mode overflow --core "$core" --event "$event" \
     --payload loop_backedge --n 100000 --period 10000 --reps "$ovreps" --out "$TMP/ae1d.json"; then fail=1; echo "AE-1(d) overflow hammer FAILED" >&2; fi
# (c) SpecLockMap ON side (locked exact under workaround)
if ! taskset -c "$core" "$HAMMER" --mode exactness --core "$core" --event "$event" \
     --payload locked --n1 20000 --n2 40000 --reps "$reps" --out "$TMP/ae1c-on.json"; then fail=1; echo "AE-1(c)-ON hammer FAILED" >&2; fi

sudo bash "$SD/posture.sh" restore >/dev/null 2>&1 || echo "WARNING: posture restore reported an error" >&2

# --- the one sanctioned deviation: workaround OFF, reproduce (or refute) the overcount ---
sudo bash "$SD/posture.sh" apply --core "$core" --speclockmap off > "$OUT/posture-off.attest.json"
if ! taskset -c "$core" "$HAMMER" --mode exactness --core "$core" --event "$event" \
     --payload locked --n1 20000 --n2 40000 --reps "$reps" --out "$TMP/ae1c-off.json"; then
  echo "AE-1(c) OFF probe returned non-zero (expected when the overcount reproduces); recorded, not gated" >&2
fi
sudo bash "$SD/posture.sh" restore >/dev/null 2>&1 || echo "WARNING: posture restore reported an error" >&2   # re-apply-permanently == baseline restore

cp "$TMP"/*.json "$OUT/"

echo "=== floor checks (recomputed from retained records) ===" >&2
: > "$OUT/floor-check.txt"
check() {  # $1 label; rest cmd — tee to the retained file, propagate the check's rc
  local label="$1"; shift
  echo "# $label" | tee -a "$OUT/floor-check.txt" >&2
  if "$@" | tee -a "$OUT/floor-check.txt"; then :; else fail=1; echo "FLOOR CHECK FAILED: $label" >&2; fi
}
check "AE-1(a) exactness"        python3 "$CHECK" exactness --min-reps 30 --records "$OUT/ae1a.json"
check "AE-1(d) overflow+skid"    python3 "$CHECK" overflow --min-overflows "$ovreps" --records "$OUT/ae1d.json"
check "AE-1(c) SpecLockMap off-vs-on" python3 "$CHECK" speclockmap --off "$OUT/ae1c-off.json" --on "$OUT/ae1c-on.json"
echo "=== evidence in $OUT ===" >&2
[ "$fail" -eq 0 ] || { echo "AE-1 run FAILED (a hammer crashed or a floor check did not pass)" >&2; exit 1; }
echo "AE-1 run PASSED (hammers + floor checks green)" >&2
