#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 N-3 condition 4: pause/resume mid-gate.
# Runs the repeat gate nested while alternately pausing the L1 QEMU with
# SIGSTOP/SIGCONT or (mode=qmp) QMP stop/cont.
#
# Re-certification fixes (PR #98 review / bead hm-b5b item 5):
#   - the cadence is PARAMETERIZED and RECORDED (the original script hardcoded
#     the known-wedging 2s-of-every-7s cadence while the accepted evidence ran
#     2s-of-every-30s, making it unreproducible from the committed harness);
#     defaults are the accepted-evidence cadence
#   - only CONFIRMED pauses count: sigstop counts a pause iff both kill -STOP
#     and kill -CONT succeeded; qmp counts a pause iff QMP acked stop AND
#     query-status showed "paused" AND acked cont AND query-status showed
#     "running" (the original `|| true` incremented the count on failed pauses)
#   - a pause attempt that fails while QEMU is still alive is recorded in
#     pauses-failed.count and fails the condition (the condition was not
#     applied as configured); attempts that fail because QEMU already exited
#     are the normal end-of-run race and are not counted either way
#
# Usage: run-n3-pause.sh <runset-name> <reps> <mode:sigstop|qmp> [pause-every-s] [pause-len-s]
set -euo pipefail

RS_NAME="${1:?runset name}"
REPS="${2:?reps}"
MODE="${3:-sigstop}"
PAUSE_EVERY="${4:-28}"   # seconds of run between pauses (accepted evidence: 2s of every ~30s)
PAUSE_LEN="${5:-2}"      # seconds paused
BASE=/root/nested-x86-spike/n3
RS="$BASE/results/$RS_NAME"
mkdir -p "$RS"

QMP_SOCK="$RS/qmp.sock"
EXTRA=""
[ "$MODE" = qmp ] && EXTRA="-qmp unix:$QMP_SOCK,server=on,wait=off"

qmp_pause_once() { # one QMP-confirmed stop/wait/cont cycle; rc 0 iff fully confirmed
  python3 - "$QMP_SOCK" "$PAUSE_LEN" <<'PYEOF'
import json, socket, sys, time

def cmd(f, execute, arguments=None):
    m = {"execute": execute}
    if arguments:
        m["arguments"] = arguments
    f.write(json.dumps(m) + "\n"); f.flush()
    # responses interleave with async events; take the first return/error
    for _ in range(50):
        line = f.readline()
        if not line:
            break
        msg = json.loads(line)
        if "return" in msg:
            return msg["return"]
        if "error" in msg:
            raise RuntimeError(msg["error"])
    raise RuntimeError("no QMP response")

s = socket.socket(socket.AF_UNIX); s.settimeout(10); s.connect(sys.argv[1])
f = s.makefile("rw")
f.readline()  # greeting
cmd(f, "qmp_capabilities")
cmd(f, "stop")
st = cmd(f, "query-status")
if st.get("status") != "paused":
    raise RuntimeError(f"stop not confirmed: {st}")
time.sleep(float(sys.argv[2]))
cmd(f, "cont")
st = cmd(f, "query-status")
if st.get("status") != "running":
    raise RuntimeError(f"cont not confirmed: {st}")
PYEOF
}

# pauser: wait for the pidfile, then pause PAUSE_LEN of every
# (PAUSE_EVERY + PAUSE_LEN) seconds until QEMU exits. Confirmed pauses only.
(
  for _ in $(seq 1 120); do [ -f "$RS/qemu.pid" ] && break; sleep 1; done
  Q=$(cat "$RS/qemu.pid" 2>/dev/null) || exit 0
  n=0
  failed=0
  echo 0 > "$RS/pauses.count"
  echo 0 > "$RS/pauses-failed.count"
  while kill -0 "$Q" 2>/dev/null; do
    sleep "$PAUSE_EVERY"
    kill -0 "$Q" 2>/dev/null || break
    ok=1
    if [ "$MODE" = qmp ]; then
      qmp_pause_once 2>>"$RS/pause-errors.log" || ok=0
    else
      if kill -STOP "$Q" 2>/dev/null; then
        sleep "$PAUSE_LEN"
        kill -CONT "$Q" 2>/dev/null || ok=0
      else
        ok=0
      fi
    fi
    if [ "$ok" = 1 ]; then
      n=$((n + 1))
      echo "$n" > "$RS/pauses.count"
    elif kill -0 "$Q" 2>/dev/null; then
      # failed while QEMU is alive: the condition was NOT applied as configured
      failed=$((failed + 1))
      echo "$failed" > "$RS/pauses-failed.count"
    fi
    # failures during the end-of-run race (QEMU already gone) count as neither
  done
) & PAUSER=$!

rc=0
QEMU_EXTRA_ARGS="$EXTRA" bash /root/nested-x86-spike/n1/src/run-appliance.sh "$RS" 28800 \
  "harmony.gates=n3_repeat_gate harmony.env=N3_REPS=$REPS,N3_ITEM=insn-rng" || rc=$?

wait "$PAUSER" 2>/dev/null || true
PAUSES=$(cat "$RS/pauses.count" 2>/dev/null || echo 0)
PAUSES_FAILED=$(cat "$RS/pauses-failed.count" 2>/dev/null || echo 0)
# a pause that failed against a live QEMU means the condition dose is not the
# recorded one — fail the condition loudly rather than report a green runset
[ "$PAUSES_FAILED" -eq 0 ] || { [ $rc -ne 0 ] || rc=3; }
{
  echo "{"
  echo "  \"condition\": \"pause-$MODE\","
  echo "  \"pause_every_s\": $PAUSE_EVERY,"
  echo "  \"pause_len_s\": $PAUSE_LEN,"
  echo "  \"pauses_confirmed\": $PAUSES,"
  echo "  \"pauses_failed\": $PAUSES_FAILED,"
  echo "  \"rc\": $rc,"
  echo "  \"finished\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/condition-end.json"
echo "N3_PAUSE_DONE mode=$MODE rc=$rc pauses=$PAUSES failed=$PAUSES_FAILED cadence=${PAUSE_LEN}s/$((PAUSE_EVERY + PAUSE_LEN))s"
exit $rc
