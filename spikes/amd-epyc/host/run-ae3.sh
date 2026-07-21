#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# run-ae3.sh — AE-3 force-exit + exact-landing orchestrator (docs/AMD-EPYC.md AE-3).
# REQUIRES the patched 6.18.35 kernel booted (host/stage-6.18-boot.sh). Pins a physical
# core, idles its SMT sibling, attests the patched-kvm_amd identity + AVIC-off posture per
# run, smoke-fires ONE armed deadline before any campaign spend, runs the exact-landing
# campaign, and machine-checks the floors from the retained per-arm records.
#
# Mechanism attestation is load-bearing (evidence integrity #4, the PR-98 lesson): the run
# refuses to start unless uname -r is the patched 6.18.35 and kvm_amd's vermagic matches;
# the harness itself forces KVM_EXIT_PREEMPT (a stock path fails every arm).
set -euo pipefail
SD=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ROOT=$(cd "$SD/.." && pwd)
HARNESS="$ROOT/harness/ae3-forceexit"
KVER=6.18.35

core=""; event=0xc4; margin=16384; arms=1000; seed=1; runset="ae3-$(uname -r)"
while [ $# -gt 0 ]; do case "$1" in
  --core) core="$2"; shift 2;; --event) event="$2"; shift 2;;
  --margin) margin="$2"; shift 2;; --arms) arms="$2"; shift 2;;
  --seed) seed="$2"; shift 2;; --runset) runset="$2"; shift 2;;
  *) echo "bad arg $1" >&2; exit 2;; esac; done
[ -n "$core" ] || { echo "need --core N" >&2; exit 2; }

# Guard: patched kernel only (a stock kernel cannot exercise the mechanism under test).
rel=$(uname -r)
[ "$rel" = "$KVER" ] || { echo "AE-3 needs the patched $KVER kernel; on $rel — boot it first" >&2; exit 3; }

OUT="$ROOT/results/ae-3/$runset"; mkdir -p "$OUT"
gcc -O2 -Wall -o "$HARNESS" "$ROOT/harness/ae3-forceexit.c"

# Guest-mode sampling perf events (the overflow PMI) need an unrestricted paranoid
# level; the fresh 6.18.35 boot resets it to the distro default (2). Scratch box under
# exclusive lock — set it for measurement (resets to default on reboot; recorded).
sudo sysctl -w kernel.perf_event_paranoid=-1 >/dev/null

echo "=== AE-3 run-set $runset on core $core, event $event, margin $margin, arms $arms ===" >&2

# posture: LS_CFG on (harmless precaution), sibling idled, AVIC-off + module identity attested
sudo bash "$SD/posture.sh" apply --core "$core" --speclockmap on > "$OUT/posture.attest.json"
# record the patched module identity alongside the posture (patched-vs-stock, evidence #4)
{ echo '{"schema":"amd-epyc-ae3-module-attest-v1",'
  echo "\"uname\":\"$rel\",\"kvm_amd_vermagic\":\"$(modinfo -F vermagic kvm_amd)\","
  echo "\"kvm_amd_srcversion\":\"$(modinfo -F srcversion kvm_amd)\","
  echo "\"avic\":\"$(cat /sys/module/kvm_amd/parameters/avic 2>/dev/null)\","
  echo "\"nested\":\"$(cat /sys/module/kvm_amd/parameters/nested 2>/dev/null)\"}"; } \
  | python3 -c 'import json,sys;print(json.dumps(json.load(sys.stdin),sort_keys=True,indent=2))' \
  > "$OUT/module.attest.json"

# --- smoke: ONE armed deadline before campaign spend (standing discipline) ---
echo "--- smoke (1 armed deadline) ---" >&2
if taskset -c "$core" "$HARNESS" --core "$core" --event "$event" --margin "$margin" --smoke \
     --out "$OUT/smoke.json"; then
  echo "smoke GO: KVM_EXIT_PREEMPT fired + landed work==target" >&2
else
  echo "smoke NO-GO: mechanism did not attest — STOP, do not spend the campaign" >&2
  sudo bash "$SD/posture.sh" restore >/dev/null 2>&1 || true
  exit 4
fi

# --- campaign: exact-landing across seeded targets, with replay determinism ---
# Gate-RC propagation (P1-2, the PR-98 green-on-fail lesson): capture the campaign's
# and the floor check's exit status; a failure of either FAILS the run. `|| true` is
# banned here — a crashed campaign or a FLOOR_CHECK: FAIL must not publish green.
echo "--- campaign ($arms arms, replay) ---" >&2
if taskset -c "$core" "$HARNESS" --core "$core" --event "$event" --margin "$margin" \
     --arms "$arms" --seed "$seed" --replay --out "$OUT/campaign.json"; then camp_rc=0; else camp_rc=$?; fi

# posture restore is cleanup (best-effort), done BEFORE propagating the measurement result
sudo bash "$SD/posture.sh" restore >/dev/null 2>&1 || echo "WARNING: posture restore reported an error" >&2

echo "=== floor check (recomputed from retained records; --min-arms enforces the campaign floor) ===" >&2
if python3 "$ROOT/schemas/check-floors.py" ae3 --records "$OUT/campaign.json" \
     --margin "$margin" --min-arms "$arms" | tee "$OUT/floor-check.txt"; then floor_rc=0; else floor_rc=${PIPESTATUS[0]}; fi

echo "=== evidence in $OUT ===" >&2
if [ "$camp_rc" -ne 0 ] || [ "$floor_rc" -ne 0 ]; then
  echo "AE-3 run FAILED (campaign rc=$camp_rc, floor-check rc=$floor_rc)" >&2; exit 1
fi
echo "AE-3 run PASSED (campaign + floor check green, >= $arms arms)" >&2
