#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 re-certification: machine-check the binding floors AGAINST THE
# RETAINED EVIDENCE (never asserted from memory) — hm-jpu/hm-60k requirement.
# Run from the repo root. Exits 0 iff every check passes; prints one line per
# check with the extracted numbers.
set -uo pipefail
R=spikes/nested-x86/results
fail=0
say() { echo "$@"; }
bad() { echo "FAIL: $@"; fail=1; }

# --- N-3: >=1000 same-seed reps bit-identical PER condition, one reference hash
REF=6163f1109b5677de0ff924f7932e7ade007434c99c24bc9a7e11beac27f5bbb4
n3_check() { # n3_check <runset> <floor> [console-file]
  local rs=$1 floor=$2 c=${3:-console.log}
  local f=$R/n3/$rs/$c
  [ -f "$f" ] || { bad "$rs: missing $c"; return; }
  local s; s=$(grep -o 'N3JSON {"event":"summary"[^}]*}' "$f" | tail -1)
  [ -n "$s" ] || { bad "$rs: no summary"; return; }
  local att idn mis sh
  att=$(echo "$s" | grep -o '"attempted":[0-9]*' | cut -d: -f2)
  idn=$(echo "$s" | grep -o '"identical":[0-9]*' | cut -d: -f2)
  mis=$(echo "$s" | grep -o '"mismatches":[0-9]*' | cut -d: -f2)
  sh=$(echo "$s" | grep -o '"state_hash":"[0-9a-f]*"' | cut -d'"' -f4)
  if [ "$att" -ge "$floor" ] && [ "$idn" = "$att" ] && [ "$mis" = 0 ] && [ "$sh" = "$REF" ]; then
    say "OK  n3/$rs: $idn/$att identical (floor $floor), hash==ref"
  else
    bad "$rs: attempted=$att identical=$idn mismatches=$mis hash=$sh"
  fi
}
n3_check solo-recert-001 1000
n3_check othercore-recert-001 1000
n3_check samecore-recert-001 1000
n3_check migrate-recert-001 1000
n3_check pause-sigstop-recert-001 1000
n3_check pause-qmp-recert-001 1000
n3_check migrate-live-recert-002 250 console-combined.log
n3_check metal-reference-recert-001 1000

# pause conditions: confirmed-only counts, zero failed
for m in sigstop qmp; do
  ce=$R/n3/pause-$m-recert-001/condition-end.json
  pf=$(grep -o '"pauses_failed": *[0-9]*' "$ce" | grep -o '[0-9]*$')
  pc=$(grep -o '"pauses_confirmed": *[0-9]*' "$ce" | grep -o '[0-9]*$')
  if [ "${pf:-1}" = 0 ] && [ "${pc:-0}" -gt 0 ]; then
    say "OK  pause-$m: $pc confirmed, 0 failed"
  else bad "pause-$m: confirmed=$pc failed=$pf"; fi
done

# migrate-live: completed + finished on destination
ce=$R/n3/migrate-live-recert-002/condition-end.json
grep -q '"migration_status": "completed"' "$ce" && grep -q '"finished_on": "destination"' "$ce" \
  && say "OK  migrate-live-recert-002: completed, finished on destination" \
  || bad "migrate-live-recert-002: $(cat $ce 2>/dev/null | tr -d '\n')"

# --- N-2: >=1,000,000 ARMED PMIs cumulative. The armed-PMI count is
# --- recomputed from records.samples — the perf-record ground truth — NEVER
# --- from a summary field the harness asserted about itself (the PR #98
# --- floor-accounting finding: the old `"armed"` summary field included
# --- MTF-only deadlines that arm no PMI, and this checker read it back).
# --- Per-deadline exactness/oracle/record-cleanliness checks still use the
# --- summary's per-run tallies (each is cross-guaranteed by the gate's rc=0
# --- assert), but the FLOOR line uses samples only.
total_deadlines=0
total_armed_pmi=0
for d in $R/n2/cond-*-recert-001 $R/n2/cond-*-topup-001 $R/n2/smoke-recert-001 $R/n2/smoke-topup-001; do
  [ -d "$d" ] || continue   # top-up runsets appear as the ruling executes
  rs=$(basename "$d")
  s=$(grep -o 'N2JSON {"event":"summary".*' "$d/console.log" | tail -1)
  [ -n "$s" ] || { bad "$rs: no N2 summary"; continue; }
  # legacy runsets emitted "armed" (which conflated the two classes); the
  # fixed hammer emits "deadlines". Read whichever names the total.
  dl=$(echo "$s" | grep -o '"deadlines":[0-9]*' | cut -d: -f2)
  [ -n "$dl" ] || dl=$(echo "$s" | grep -o '"armed":[0-9]*' | cut -d: -f2)
  exact=$(echo "$s" | grep -o '"exact":[0-9]*' | cut -d: -f2)
  ok=$(echo "$s" | grep -o '"oracle_ok":[0-9]*' | cut -d: -f2)
  rv=$(echo "$s" | grep -o '"record_violations":[0-9]*' | cut -d: -f2)
  samples=$(echo "$s" | grep -o '"samples":[0-9]*' | cut -d: -f2)
  lost=$(echo "$s" | grep -o '"lost":[0-9]*' | cut -d: -f2)
  thr=$(echo "$s" | grep -o '"throttle":[0-9]*' | cut -d: -f2)
  backend=$(grep -o '"backend":"[A-Za-z]*"' "$d/console.log" | head -1 | cut -d'"' -f4)
  if [ "$exact" = "$dl" ] && [ "$ok" = "$dl" ] && [ "$rv" = 0 ] \
     && [ "$lost" = 0 ] && [ "$thr" = 0 ] && [ "$backend" = PatchedKvmBackend ]; then
    say "OK  n2/$rs: $exact/$dl deadlines exact, oracle==exact, armed PMIs (from records)=$samples, records clean, $backend"
    total_deadlines=$((total_deadlines + dl))
    total_armed_pmi=$((total_armed_pmi + samples))
  else
    bad "$rs: deadlines=$dl exact=$exact oracle=$ok rv=$rv lost=$lost throttle=$thr backend=$backend"
  fi
done
say "n2 cumulative deadlines driven: $total_deadlines (informational — NOT the floor axis)"
if [ "$total_armed_pmi" -ge 1000000 ]; then
  say "OK  n2 cumulative ARMED PMIs (from records.samples): $total_armed_pmi >= 1,000,000"
else
  bad "n2 cumulative ARMED PMIs (from records.samples): $total_armed_pmi < 1,000,000 — floor UNMET; ruling pending (top-up vs criterion revision)"
fi

# --- cross-substrate: nested control final_work == metal hammer final_work
nw=$(grep -o '"final_work":[0-9]*' $R/n2/cond-idle-control10k-recert-001/console.log | tail -1 | cut -d: -f2)
mw=$(grep -o '"final_work":[0-9]*' $R/n3/metal-reference-recert-001/console.log 2>/dev/null | tail -1 | cut -d: -f2)
if [ -n "$nw" ] && [ "$nw" = "$mw" ]; then say "OK  nested==metal final_work: $nw"
else bad "final_work nested=$nw metal=$mw"; fi
# and the smoke pair
nsw=$(grep -o '"final_work":[0-9]*' $R/n2/smoke-recert-001/console.log | tail -1 | cut -d: -f2)
msw=$(grep -o '"final_work":[0-9]*' $R/n3/metal-smoke-recert-001/console.log 2>/dev/null | tail -1 | cut -d: -f2)
if [ -n "$nsw" ] && [ "$nsw" = "$msw" ]; then say "OK  nested==metal smoke final_work: $nsw"
else bad "smoke final_work nested=$nsw metal=$msw"; fi

[ $fail = 0 ] && echo "ALL FLOORS MACHINE-CHECKED: PASS" || echo "FLOOR CHECK: FAILURES PRESENT"
exit $fail
