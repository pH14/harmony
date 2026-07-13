#!/bin/bash
# N-3 conditions 2-3: repeat gate under L0 stress / migration (reuses N-2 condition shapes)
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
    Q=$(cat "$RS/qemu.pid" 2>/dev/null) || exit 0; i=0
    while kill -0 "$Q" 2>/dev/null; do
      taskset -a -pc $(( (i * 5) % 16 )) "$Q" >/dev/null 2>&1 || true
      i=$((i+1)); sleep 0.5
    done; echo "$i" > "$RS/migrations.count" ) & MIGRATOR_PID=$!
fi
rc=0
CPUSET_OVERRIDE=$CPUSET_OVERRIDE bash /root/nested-x86-spike/n1/src/run-appliance.sh "$RS" 28800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=$REPS,N3_ITEM=insn-rng,N3_PROGRESS=25" || rc=$?
[ -n "$STRESS_PID" ] && kill "$STRESS_PID" 2>/dev/null || true
[ -n "$MIGRATOR_PID" ] && wait "$MIGRATOR_PID" 2>/dev/null || true
echo "{\"finished\":\"$(date -u +%FT%TZ)\",\"rc\":$rc}" > "$RS/condition-end.json"
echo "N3_STRESS_DONE $COND rc=$rc"
exit $rc
