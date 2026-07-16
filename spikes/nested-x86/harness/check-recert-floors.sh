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

# --- N-3: >=1000 same-seed reps bit-identical PER condition, one reference PAIR
# (round-4 P1: the gate emits and compares BOTH digests, so both are pinned —
# a run with the right state_hash but a wrong observable_digest must fail here).
REF=6163f1109b5677de0ff924f7932e7ade007434c99c24bc9a7e11beac27f5bbb4
OREF=0fe06bf4edf727fc1d200f810a307c30e65915b7c1ed230a5e513defbb2a3926
n3_check() { # n3_check <runset> <floor> [console-file]
  local rs=$1 floor=$2 c=${3:-console.log}
  local f=$R/n3/$rs/$c
  [ -f "$f" ] || { bad "$rs: missing $c"; return; }
  local s; s=$(grep -o 'N3JSON {"event":"summary"[^}]*}' "$f" | tail -1)
  [ -n "$s" ] || { bad "$rs: no summary"; return; }
  local att idn mis sh od
  att=$(echo "$s" | grep -o '"attempted":[0-9]*' | cut -d: -f2)
  idn=$(echo "$s" | grep -o '"identical":[0-9]*' | cut -d: -f2)
  mis=$(echo "$s" | grep -o '"mismatches":[0-9]*' | cut -d: -f2)
  sh=$(echo "$s" | grep -o '"state_hash":"[0-9a-f]*"' | cut -d'"' -f4)
  od=$(echo "$s" | grep -o '"observable_digest":"[0-9a-f]*"' | cut -d'"' -f4)
  # round-7 P1: the recorded RC EVIDENCE is required too — a summary line can
  # precede a post-summary gate failure, so counts+hashes alone never suffice.
  case "$rs" in
    metal-*)
      # metal session: every METAL_GATE that began must have an rc line, all 0
      local mb mr mf
      mb=$(grep -c "METAL_GATE_BEGIN" "$f" 2>/dev/null || true)
      mr=$(grep -c "METAL_GATE_RC" "$f" 2>/dev/null || true)
      mf=$(grep -c "METAL_GATE_RC .* rc=[1-9]" "$f" 2>/dev/null || true)
      { [ "$mb" -gt 0 ] && [ "$mr" -eq "$mb" ] && [ "$mf" -eq 0 ]; } \
        || { bad "$rs: metal gate RCs (began=$mb rc_lines=$mr failing=$mf)"; return; }
      # round-9 P1: the recorded RESTORE artifact is required too. Scope
      # honestly: a restore failure is box hygiene AFTER evidence production
      # (the gates above already ran and recorded), not retroactive taint on
      # the measurements — but the L0 swap discipline demands the artifact
      # exist and show stock+nested restored. The retained run's window-close
      # check (RESTORE_VERIFIED_IDENTICAL vs box-restore-manifest-recert.json)
      # is the module-hash-level confirmation on top of this per-runset file.
      local rst=$R/n3/$rs/env.json.restore
      [ -f "$rst" ] || { bad "$rs: env.json.restore missing (restore artifact required)"; return; }
      grep -qE "^kvm +1396736 " "$rst" || { bad "$rs: restore artifact lacks stock kvm 1396736"; return; }
      grep -q "nested=Y" "$rst" || { bad "$rs: restore artifact lacks nested=Y"; return; } ;;
    migrate-live-*)
      # boots its own QEMUs; the recorded rc is the wrapper's condition-end rc
      local mlrc; mlrc=$(grep -o '"rc": *[0-9]*' "$R/n3/$rs/condition-end.json" 2>/dev/null | grep -o '[0-9]*$' | tail -1)
      [ "${mlrc:-1}" = 0 ] || { bad "$rs: wrapper rc=$mlrc"; return; } ;;
    *)
      # appliance-boot runsets: the retained QEMU exit code must be 0
      local qrc; qrc=$(grep -o 'qemu_rc=[0-9]*' "$R/n3/$rs/env.json.rc" 2>/dev/null | cut -d= -f2)
      [ "${qrc:-1}" = 0 ] || { bad "$rs: qemu_rc=${qrc:-missing}"; return; } ;;
  esac
  # condition-dose evidence (round-4 P1): stress/migrate runsets must carry it.
  # Round-4+ harnesses record liveness + successful-migration counts and are
  # enforced; the retained recert runsets predate the fields — their dose is
  # PROVEN from recorded artifacts in results/AUDIT-2026-07-12.md §"N-3 dose
  # audit (round-4)" (sustained slowdown ratios; full-duration migrator with
  # attempts-era count), which the annotation cites rather than silently passes.
  local dose=""
  case "$rs" in
    migrate-live-*) ;; # the QEMU live-migration rehearsal: its dose evidence is
                       # the migration_status/finished_on check further down,
                       # not taskset affinity counts
    othercore-*|samecore-*|migrate-*)
      local ce=$R/n3/$rs/condition-end.json
      [ -f "$ce" ] || { bad "$rs: condition-end.json missing"; return; }
      local cerc; cerc=$(grep -o '"rc": *[0-9]*' "$ce" | grep -o '[0-9]*$' | tail -1)
      [ "${cerc:-1}" = 0 ] || { bad "$rs: condition rc=$cerc"; return; }
      if grep -q '"stressor_alive_at_end"' "$ce"; then
        local sal mg mf
        sal=$(grep -o '"stressor_alive_at_end": *"[a-z/]*"' "$ce" | cut -d'"' -f4)
        mg=$(grep -o '"migrations": *[0-9]*' "$ce" | grep -o '[0-9]*$')
        mf=$(grep -o '"migrations_failed": *[0-9]*' "$ce" | grep -o '[0-9]*$')
        [ "$sal" != "no" ] || { bad "$rs: stressor died mid-run"; return; }
        case "$rs" in migrate-*)
          [ "${mf:-0}" = 0 ] || { bad "$rs: migrations_failed=$mf"; return; }
          [ "${mg:-0}" -gt 0 ] || { bad "$rs: zero successful migrations"; return; } ;;
        esac
        dose="; dose verified (liveness/migrations fields)"
      else
        case "$rs" in migrate-*)
          local mc; mc=$(cat "$R/n3/$rs/migrations.count" 2>/dev/null || echo 0)
          [ "${mc:-0}" -gt 0 ] || { bad "$rs: no migrations.count evidence"; return; }
          dose="; dose: legacy, proven in audit note (migrator $mc iters, full-duration)" ;;
        *)
          dose="; dose: legacy, proven in audit note (sustained slowdown vs solo)" ;;
        esac
      fi ;;
  esac
  if [ "$att" -ge "$floor" ] && [ "$idn" = "$att" ] && [ "$mis" = 0 ] \
     && [ "$sh" = "$REF" ] && [ "$od" = "$OREF" ]; then
    say "OK  n3/$rs: $idn/$att identical (floor $floor), state_hash==ref, observable_digest==ref$dose"
  else
    bad "$rs: attempted=$att identical=$idn mismatches=$mis hash=$sh od=$od"
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
  # PR #98 round-3 #1: a runset counts toward the floor ONLY after its own
  # condition-applied evidence is machine-checked, not just its summary line.
  # cond-* runsets must carry condition-end.json with rc 0; where the round-2
  # fields exist they are enforced (a dead stressor or failed migration means
  # the condition label is false); their absence in pre-round-2 runsets is
  # ANNOTATED, never silently equated with passing them. Smoke runsets run
  # bare run-appliance (no condition file) and must show qemu_rc=0.
  cond_note=""
  cond_ok=1
  case "$rs" in
    cond-*)
      ce="$d/condition-end.json"
      if [ ! -f "$ce" ]; then
        cond_ok=0; cond_note="condition-end.json MISSING"
      else
        cerc=$(grep -o '"rc": *[0-9]*' "$ce" | grep -o '[0-9]*$' | tail -1)
        [ "${cerc:-1}" = 0 ] || { cond_ok=0; cond_note="condition rc=$cerc"; }
        if grep -q '"stressor_alive_at_end"' "$ce"; then
          sal=$(grep -o '"stressor_alive_at_end": *"[a-z/]*"' "$ce" | cut -d'"' -f4)
          mf=$(grep -o '"migrations_failed": *[0-9]*' "$ce" | grep -o '[0-9]*$')
          [ "$sal" != "no" ] || { cond_ok=0; cond_note="stressor died mid-run"; }
          [ "${mf:-0}" = 0 ] || { cond_ok=0; cond_note="migrations_failed=$mf"; }
          [ -n "$cond_note" ] || cond_note="condition-end ok (liveness+migrations checked)"
        else
          cond_note="condition-end rc=0 (legacy: pre-round-2, no liveness fields recorded)"
        fi
        # round-4 P2: a migrate condition requires its dose — >0 successful (or,
        # for attempts-era runsets, recorded) affinity changes in migrations.count
        case "$rs" in cond-migrate-*)
          mc=$(cat "$d/migrations.count" 2>/dev/null || echo 0)
          if [ "${mc:-0}" -gt 0 ]; then
            cond_note="$cond_note; migrations.count=$mc"
          else
            cond_ok=0; cond_note="migrate condition with zero recorded migrations"
          fi ;;
        esac
      fi ;;
    smoke-*)
      qrc=$(grep -o 'qemu_rc=[0-9]*' "$d/env.json.rc" 2>/dev/null | cut -d= -f2)
      [ "${qrc:-1}" = 0 ] || { cond_ok=0; cond_note="qemu_rc=$qrc"; }
      [ -n "$cond_note" ] || cond_note="smoke (qemu_rc=0)" ;;
  esac
  # round-5 P1: records.samples must be PRESENT and numeric — a summary missing
  # it would otherwise contribute a silent zero to the floor accumulation while
  # the runset is accepted (no verified PMI accounting at all).
  case "$samples" in ''|*[!0-9]*) bad "$rs: records.samples missing/non-numeric in summary"; continue ;; esac
  if [ "$cond_ok" = 1 ] && [ "$exact" = "$dl" ] && [ "$ok" = "$dl" ] && [ "$rv" = 0 ] \
     && [ "$lost" = 0 ] && [ "$thr" = 0 ] && [ "$backend" = PatchedKvmBackend ]; then
    say "OK  n2/$rs: $exact/$dl deadlines exact, oracle==exact, armed PMIs (from records)=$samples, records clean, $backend; $cond_note"
    total_deadlines=$((total_deadlines + dl))
    total_armed_pmi=$((total_armed_pmi + samples))
  else
    bad "$rs: deadlines=$dl exact=$exact oracle=$ok rv=$rv lost=$lost throttle=$thr backend=$backend cond=[$cond_note]"
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
