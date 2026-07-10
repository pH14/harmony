#!/bin/bash
# nested-x86 N-2: run the deadline hammer nested under ONE L0 condition.
# Usage: run-n2-condition.sh <condition> <deadlines> <runset-name> [seed] [gates]
#   condition: idle | othercore | samecore | mempress | timerstorm | migrate
# The L0 condition runs for the WHOLE appliance lifetime (clean accounting).
set -euo pipefail

COND="${1:?condition}"
DEADLINES="${2:?deadlines}"
RS_NAME="${3:?runset name}"
SEED="${4:-1600065574}"   # 0x5F5E_2026 decimal-safe default; hammer default if empty
GATES="${5:-n2_nested_hammer}"
BASE=/root/nested-x86-spike/n2
RS="$BASE/results/$RS_NAME"
mkdir -p "$RS"

STRESS_PID=""
CPUSET_OVERRIDE=3
MIGRATOR_PID=""
case "$COND" in
  idle) ;;
  othercore)  taskset -c 0-2,4-10,12-15 stress-ng --cpu 12 --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  samecore)   taskset -c 3,11 stress-ng --cpu 2 --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  mempress)   taskset -c 0-2,4-10,12-15 stress-ng --vm 4 --vm-bytes 16G --vm-keep --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  timerstorm) taskset -c 3,11 stress-ng --timer 4 --timer-freq 25000 --timeout 28800 >/dev/null 2>&1 & STRESS_PID=$! ;;
  migrate)    CPUSET_OVERRIDE=0-15 ;;
  *) echo "unknown condition $COND"; exit 2 ;;
esac

{
  echo "{"
  echo "  \"condition\": \"$COND\","
  echo "  \"deadlines\": $DEADLINES,"
  echo "  \"seed\": $SEED,"
  echo "  \"stress_pid\": \"$STRESS_PID\","
  echo "  \"cpuset\": \"$CPUSET_OVERRIDE\","
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/condition.json"

if [ "$COND" = migrate ]; then
  # move every QEMU thread to a new core every 500ms, keyed on the pidfile
  (
    for _ in $(seq 1 120); do [ -f "$RS/qemu.pid" ] && break; sleep 1; done
    Q=$(cat "$RS/qemu.pid" 2>/dev/null) || exit 0
    i=0
    while kill -0 "$Q" 2>/dev/null; do
      core=$(( (i * 5) % 16 ))
      taskset -a -pc "$core" "$Q" >/dev/null 2>&1 || true
      i=$((i + 1))
      sleep 0.5
    done
    echo "$i" > "$RS/migrations.count"
  ) & MIGRATOR_PID=$!
fi

rc=0
CPUSET_OVERRIDE=$CPUSET_OVERRIDE bash /root/nested-x86-spike/n1/src/run-appliance.sh "$RS" 28800 \
  "harmony.gates=$GATES harmony.env=N2_DEADLINES=$DEADLINES,N2_SEED=$SEED,N2_PROGRESS=25000" \
  || rc=$?

[ -n "$STRESS_PID" ] && kill "$STRESS_PID" 2>/dev/null || true
[ -n "$MIGRATOR_PID" ] && wait "$MIGRATOR_PID" 2>/dev/null || true
echo "{\"finished\": \"$(date -u +%FT%TZ)\", \"rc\": $rc}" > "$RS/condition-end.json"
echo "N2_CONDITION_DONE $COND rc=$rc"
exit $rc
