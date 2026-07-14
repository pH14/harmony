#!/bin/bash
# nested-x86 N-3 condition 5: cloud-migration rehearsal — QEMU local live
# migration of the running L1 mid-gate. Pass criterion: the gate completes
# with rc=0 on the DESTINATION after a completed migration, OR the migration
# fails closed (failed/cancelled) and the gate completes with rc=0 on the
# SOURCE — never silent divergence, never an unconditional success exit.
#
# Re-certification fixes (PR #98 review / bead hm-b5b item 5): the original
# script exited 0 unconditionally; success now requires L1_DONE + zero failing
# gate RCs + a machine-readable gate summary in the console the guest actually
# finished on, consistent with the recorded migration status. Boot artifacts
# are hash-verified against the appliance build manifest BEFORE boot and the
# manifest is retained with the runset.
#
# Usage: run-n3-migrate-live.sh <runset-name> <reps> [migrate-after-seconds]
set -euo pipefail

RS_NAME="${1:?runset name}"
REPS="${2:?reps}"
MIG_AFTER="${3:-90}"
BASE=/root/nested-x86-spike/n3
RS="$BASE/results/$RS_NAME"
KVER=6.12.90+deb13.1-amd64
APPL_BASE=/root/nested-x86-spike/n1
APPL=$APPL_BASE/appliance.cpio.gz
QEMU=/usr/bin/qemu-system-x86_64
mkdir -p "$RS"

# hash-verify before boot (recording is not verifying) + retain the manifest
MANIFEST="$APPL_BASE/build-manifest.json"
[ -f "$MANIFEST" ] || { echo "PIN MANIFEST MISSING: $MANIFEST"; exit 1; }
pin_get() { grep -o "\"$1\": \"[0-9a-f]*\"" "$MANIFEST" | head -1 | cut -d'"' -f4; }
pin_verify() { # pin_verify <file> <want-sha256> <label>
  local got; got=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $3 ($1): got $got want $2"; exit 1; }
}
WANT_APPL=$(pin_get sha256_appliance_cpio)
WANT_KERN=$(pin_get sha256_l1_kernel)
[ -n "$WANT_APPL" ] && [ -n "$WANT_KERN" ] || { echo "PIN MANIFEST INCOMPLETE: $MANIFEST"; exit 1; }
pin_verify "$APPL" "$WANT_APPL" appliance-initrd
pin_verify "/boot/vmlinuz-$KVER" "$WANT_KERN" l1-kernel
cp "$MANIFEST" "$RS/build-manifest.json"
echo "PIN_VERIFIED appliance=$WANT_APPL kernel=$WANT_KERN"

{
  echo "{"
  echo "  \"condition\": \"live-migration rehearsal\","
  echo "  \"qemu_sha256\": \"$(sha256sum $QEMU | cut -d' ' -f1)\","
  echo "  \"kernel_sha256\": \"$(sha256sum /boot/vmlinuz-$KVER | cut -d' ' -f1)\","
  echo "  \"initrd_sha256\": \"$(sha256sum $APPL | cut -d' ' -f1)\","
  echo "  \"reps\": $REPS,"
  echo "  \"migrate_after_s\": $MIG_AFTER,"
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/env.json"

APPEND="console=ttyS0 rdinit=/init panic=-1 harmony.gates=n3_repeat_gate harmony.env=N3_REPS=$REPS,N3_ITEM=insn-rng"

# destination first (listens), pinned to core 5 (source keeps core 3)
taskset -c 5 $QEMU \
    -machine q35,accel=kvm -cpu host,pmu=on -smp 1 -m 8192 \
    -kernel /boot/vmlinuz-$KVER -initrd "$APPL" -append "$APPEND" \
    -display none -monitor none -no-reboot \
    -pidfile "$RS/qemu-dst.pid" \
    -serial "file:$RS/console-dst.log" \
    -qmp "unix:$RS/qmp-dst.sock,server=on,wait=off" \
    -incoming "unix:$RS/mig.sock" \
    </dev/null >"$RS/qemu-dst-stdout.log" 2>&1 & DST=$!

# source
taskset -c 3 $QEMU \
    -machine q35,accel=kvm -cpu host,pmu=on -smp 1 -m 8192 \
    -kernel /boot/vmlinuz-$KVER -initrd "$APPL" -append "$APPEND" \
    -display none -monitor none -no-reboot \
    -pidfile "$RS/qemu-src.pid" \
    -serial "file:$RS/console-src.log" \
    -qmp "unix:$RS/qmp-src.sock,server=on,wait=off" \
    </dev/null >"$RS/qemu-src-stdout.log" 2>&1 & SRC=$!

sleep "$MIG_AFTER"

python3 - "$RS/qmp-src.sock" "$RS/mig.sock" > "$RS/migration.json" 2>&1 <<'PYEOF'
import json, socket, sys, time

def qmp(sock_path):
    s = socket.socket(socket.AF_UNIX); s.settimeout(30); s.connect(sock_path)
    f = s.makefile('rw')
    f.readline()
    f.write(json.dumps({"execute": "qmp_capabilities"}) + "\n"); f.flush(); f.readline()
    return f

f = qmp(sys.argv[1])
f.write(json.dumps({"execute": "migrate", "arguments": {"uri": "unix:" + sys.argv[2]}}) + "\n")
f.flush(); print(f.readline().strip())
deadline = time.time() + 600
status = "unknown"
while time.time() < deadline:
    f.write(json.dumps({"execute": "query-migrate"}) + "\n"); f.flush()
    for _ in range(20):
        line = f.readline()
        if '"return"' in line or '"error"' in line:
            break
    try:
        status = json.loads(line)["return"]["status"]
    except Exception:
        pass
    print(json.dumps({"poll": status, "t": time.time()}))
    if status in ("completed", "failed", "cancelled"):
        break
    time.sleep(2)
print(json.dumps({"final": status}))
PYEOF

MIG_STATUS=$(grep -o '"final": "[a-z]*"' "$RS/migration.json" | cut -d'"' -f4 || echo unknown)
echo "MIGRATION_STATUS=$MIG_STATUS"

# wait (bounded) for the gate to finish wherever the guest now runs
for _ in $(seq 1 480); do
  grep -q "NESTED_X86_L1_DONE" "$RS/console-dst.log" 2>/dev/null && break
  grep -q "NESTED_X86_L1_DONE" "$RS/console-src.log" 2>/dev/null && break
  sleep 10
done

# teardown whichever QEMU is still alive (source sits paused in postmigrate)
for P in "$RS/qemu-src.pid" "$RS/qemu-dst.pid"; do
  [ -f "$P" ] && kill "$(cat "$P")" 2>/dev/null || true
done
wait "$SRC" "$DST" 2>/dev/null || true

# --- verdict. The guest's serial stream SPLITS at the migration point (e.g.
# --- GATE_BEGIN lands on the source console, GATE_RC + L1_DONE on the
# --- destination), so gate accounting is evaluated over the CONCATENATED
# --- consoles; where the guest FINISHED (L1_DONE) must match the recorded
# --- migration status:
#   completed        -> L1_DONE on the DESTINATION + combined gates green
#   failed/cancelled -> fail-closed path: L1_DONE on the SOURCE + combined green
#   unknown          -> the rehearsal did not produce a status: fail
COMBINED="$RS/console-combined.log"
cat "$RS/console-src.log" "$RS/console-dst.log" > "$COMBINED" 2>/dev/null || true

gates_green() { # gates_green <combined-console> — >=1 gate + RC-per-BEGIN, all 0 + summary
  local c=$1
  grep -q "NESTED_X86_L1_DONE" "$c" 2>/dev/null || return 1
  local began rcs fails summ
  began=$(grep -c "NESTED_X86_GATE_BEGIN" "$c" 2>/dev/null || true)
  rcs=$(grep -c "NESTED_X86_GATE_RC " "$c" 2>/dev/null || true)
  fails=$(grep -c "NESTED_X86_GATE_RC .* rc=[1-9]" "$c" 2>/dev/null || true)
  summ=$(grep -c 'N3JSON {"event":"summary"' "$c" 2>/dev/null || true)
  [ "$began" -gt 0 ] && [ "$rcs" -eq "$began" ] && [ "$fails" -eq 0 ] && [ "$summ" -gt 0 ]
}

rc=1
FINISHED_ON=none
case "$MIG_STATUS" in
  completed)
    if grep -q "NESTED_X86_L1_DONE" "$RS/console-dst.log" 2>/dev/null && gates_green "$COMBINED"; then
      rc=0; FINISHED_ON=destination
    fi ;;
  failed|cancelled)
    # fail-closed: migration refused/aborted, guest must have continued on source
    if grep -q "NESTED_X86_L1_DONE" "$RS/console-src.log" 2>/dev/null && gates_green "$COMBINED"; then
      rc=0; FINISHED_ON=source
    fi ;;
  *)
    rc=1 ;;
esac

SUMMARY=$(grep -h -o 'N3JSON {"event":"summary".*' "$RS/console-dst.log" "$RS/console-src.log" 2>/dev/null | tail -1 | tr -d '\r' || true)
{
  echo "{"
  echo "  \"migration_status\": \"$MIG_STATUS\","
  echo "  \"finished_on\": \"$FINISHED_ON\","
  echo "  \"rc\": $rc,"
  if [ -n "$SUMMARY" ]; then
    echo "  \"gate_summary\": ${SUMMARY#N3JSON },"
  else
    echo "  \"gate_summary\": null,"
  fi
  echo "  \"finished\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/condition-end.json"
echo "N3_MIGRATE_LIVE_DONE status=$MIG_STATUS finished_on=$FINISHED_ON rc=$rc"
exit $rc
