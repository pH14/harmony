#!/bin/bash
# nested-x86 N-3 condition 4: pause/resume mid-gate.
# Runs the repeat gate nested while alternately pausing the L1 QEMU with
# SIGSTOP/SIGCONT and (if mode=qmp) QMP stop/cont. Every pause is counted.
# Usage: run-n3-pause.sh <runset-name> <reps> <mode:sigstop|qmp>
set -euo pipefail

RS_NAME="${1:?runset name}"
REPS="${2:?reps}"
MODE="${3:-sigstop}"
BASE=/root/nested-x86-spike/n3
RS="$BASE/results/$RS_NAME"
mkdir -p "$RS"

QMP_SOCK="$RS/qmp.sock"
EXTRA=""
[ "$MODE" = qmp ] && EXTRA="-qmp unix:$QMP_SOCK,server=on,wait=off"

# pauser: wait for the pidfile, then pause 2s of every 7s until QEMU exits
(
  for _ in $(seq 1 120); do [ -f "$RS/qemu.pid" ] && break; sleep 1; done
  Q=$(cat "$RS/qemu.pid" 2>/dev/null) || exit 0
  n=0
  while kill -0 "$Q" 2>/dev/null; do
    sleep 5
    if [ "$MODE" = qmp ]; then
      python3 - "$QMP_SOCK" <<'PYEOF' 2>/dev/null || true
import json,socket,sys,time
s=socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1])
f=s.makefile('rw')
f.readline(); f.write(json.dumps({"execute":"qmp_capabilities"})+"\n"); f.flush(); f.readline()
f.write(json.dumps({"execute":"stop"})+"\n"); f.flush(); f.readline()
time.sleep(2)
f.write(json.dumps({"execute":"cont"})+"\n"); f.flush(); f.readline()
PYEOF
    else
      kill -STOP "$Q" 2>/dev/null || break
      sleep 2
      kill -CONT "$Q" 2>/dev/null || break
    fi
    n=$((n + 1))
    echo "$n" > "$RS/pauses.count"
  done
) & PAUSER=$!

rc=0
QEMU_EXTRA_ARGS="$EXTRA" bash /root/nested-x86-spike/n1/src/run-appliance.sh "$RS" 28800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=$REPS,N3_ITEM=insn-rng" || rc=$?

wait "$PAUSER" 2>/dev/null || true
echo "{\"condition\": \"pause-$MODE\", \"pauses\": $(cat "$RS/pauses.count" 2>/dev/null || echo 0), \"rc\": $rc, \"finished\": \"$(date -u +%FT%TZ)\"}" > "$RS/condition-end.json"
echo "N3_PAUSE_DONE mode=$MODE rc=$rc pauses=$(cat "$RS/pauses.count" 2>/dev/null || echo 0)"
exit $rc
