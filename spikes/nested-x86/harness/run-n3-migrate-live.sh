#!/bin/bash
# nested-x86 N-3 condition 5: cloud-migration rehearsal — QEMU local live
# migration of the running L1 mid-gate. Pass criterion: the gate completes
# bit-identically on the destination OR the migration fails closed (refused /
# aborted, guest continues on source) — never silent divergence.
# Usage: run-n3-migrate-live.sh <runset-name> <reps> [migrate-after-seconds]
set -euo pipefail

RS_NAME="${1:?runset name}"
REPS="${2:?reps}"
MIG_AFTER="${3:-90}"
BASE=/root/nested-x86-spike/n3
RS="$BASE/results/$RS_NAME"
KVER=6.12.90+deb13.1-amd64
APPL=/root/nested-x86-spike/n1/appliance.cpio.gz
QEMU=/usr/bin/qemu-system-x86_64
mkdir -p "$RS"

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

echo "{\"migration_status\": \"$MIG_STATUS\", \"finished\": \"$(date -u +%FT%TZ)\"}" > "$RS/condition-end.json"
echo "N3_MIGRATE_LIVE_DONE status=$MIG_STATUS"
