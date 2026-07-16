#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# N-3 conditions 2-3: repeat gate under L0 stress / migration (reuses N-2 condition shapes).
# Promoted (PR #98 round-3 #3) from harness/box-retrieved/run-n3-stress.sh — the
# authoritative script the N-3 recert drivers invoke at
# /root/nested-x86-spike/run-n3-stress.sh (see the staging map in ../README.md);
# box-retrieved/ keeps the as-run provenance copy of the retained runsets.
#
# Round-4 P1 fixes (forward; the retained runsets' dose is audited from their
# recorded artifacts in results/AUDIT-2026-07-12.md):
#   - stressor liveness: a stress generator that died before the gate ended
#     means the condition label is false — recorded + the condition FAILS (rc=5)
#   - migrations count SUCCESSFUL taskset affinity changes only; failures
#     against a live QEMU are counted and fail the condition (rc=6); a
#     zero-success migrate run fails (rc=7) — the dose must have happened
set -euo pipefail
COND="${1:?condition}"; REPS="${2:?reps}"; RS_NAME="${3:?runset}"
BASE=/root/nested-x86-spike/n3; RS="$BASE/results/$RS_NAME"; mkdir -p "$RS"
STRESS_PID=""; CPUSET_OVERRIDE=3; MIGRATOR_PID=""
case "$COND" in
  othercore)  taskset -c 0-2,4-10,12-15 stress-ng --cpu 12 --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  samecore)   taskset -c 3,11 stress-ng --cpu 2 --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  migrate)    CPUSET_OVERRIDE=0-15 ;;
  *) echo "unknown condition $COND"; exit 2 ;;
esac
echo "{\"condition\":\"$COND\",\"reps\":$REPS,\"stress_pid\":\"$STRESS_PID\",\"cpuset\":\"$CPUSET_OVERRIDE\",\"started\":\"$(date -u +%FT%TZ)\"}" > "$RS/condition.json"
if [ "$COND" = migrate ]; then
  ( for _ in $(seq 1 120); do [ -f "$RS/qemu.pid" ] && break; sleep 1; done
    Q=$(cat "$RS/qemu.pid" 2>/dev/null) || exit 0
    i=0; n=0; failed=0
    echo 0 > "$RS/migrations.count"; echo 0 > "$RS/migrations-failed.count"
    while kill -0 "$Q" 2>/dev/null; do
      if taskset -a -pc $(( (i * 5) % 16 )) "$Q" >/dev/null 2>&1; then
        n=$((n+1)); echo "$n" > "$RS/migrations.count"
      elif kill -0 "$Q" 2>/dev/null; then
        failed=$((failed+1)); echo "$failed" > "$RS/migrations-failed.count"
      fi
      i=$((i+1)); sleep 0.5
    done ) & MIGRATOR_PID=$!
fi
rc=0
CPUSET_OVERRIDE=$CPUSET_OVERRIDE bash /root/nested-x86-spike/n1/src/run-appliance.sh "$RS" 28800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=$REPS,N3_ITEM=insn-rng,N3_PROGRESS=25" || rc=$?
STRESS_ALIVE=n/a
if [ -n "$STRESS_PID" ]; then
  if kill -0 "$STRESS_PID" 2>/dev/null; then STRESS_ALIVE=yes; else
    STRESS_ALIVE=no
    echo "N3_STRESS_STRESSOR_DIED $COND pid=$STRESS_PID"
    [ $rc -ne 0 ] || rc=5
  fi
  kill "$STRESS_PID" 2>/dev/null || true
fi
[ -n "$MIGRATOR_PID" ] && wait "$MIGRATOR_PID" 2>/dev/null || true
MIGS=$(cat "$RS/migrations.count" 2>/dev/null || echo 0)
MIG_FAILED=$(cat "$RS/migrations-failed.count" 2>/dev/null || echo 0)
if [ "$COND" = migrate ]; then
  [ "$MIG_FAILED" -eq 0 ] || { echo "N3_STRESS_MIGRATIONS_FAILED $MIG_FAILED"; [ $rc -ne 0 ] || rc=6; }
  [ "$MIGS" -gt 0 ] || { echo "N3_STRESS_NO_MIGRATIONS"; [ $rc -ne 0 ] || rc=7; }
fi
{
  echo "{"
  echo "  \"finished\": \"$(date -u +%FT%TZ)\","
  echo "  \"stressor_alive_at_end\": \"$STRESS_ALIVE\","
  echo "  \"migrations\": $MIGS,"
  echo "  \"migrations_failed\": $MIG_FAILED,"
  echo "  \"rc\": $rc"
  echo "}"
} > "$RS/condition-end.json"
echo "N3_STRESS_DONE $COND rc=$rc stressor=$STRESS_ALIVE migrations=$MIGS/-$MIG_FAILED"
exit $rc
