#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86: boot the consonance appliance under stock-KVM L0. One run-set.
# Usage: run-appliance.sh <results-dir> [timeout-seconds] [extra-cmdline]
#   extra-cmdline e.g. "harmony.gates=n2_nested_hammer harmony.env=N2_DEADLINES=250000"
set -euo pipefail

BASE="${APPLIANCE_BASE:-/root/nested-x86-spike/n1}"
KVER=6.12.90+deb13.1-amd64
RS="${1:?results dir required}"
case "$RS" in /*) ;; *) RS="$BASE/results/$RS" ;; esac
TIMEOUT="${2:-1800}"
EXTRA_CMDLINE="${3:-}"
mkdir -p "$RS"

QEMU=/usr/bin/qemu-system-x86_64
CPUSET="${CPUSET_OVERRIDE:-3}"   # pinned per box core discipline; override for the migration condition

# --- hash-verify every boot artifact BEFORE boot (recording is not verifying;
# --- PR #98 review / bead hm-b5b item 3) against the appliance build manifest,
# --- and retain that manifest with the runset so provenance is committed.
MANIFEST="$BASE/build-manifest.json"
[ -f "$MANIFEST" ] || { echo "PIN MANIFEST MISSING: $MANIFEST (run build-appliance.sh first)"; exit 1; }
pin_get() { grep -o "\"$1\": \"[0-9a-f]*\"" "$MANIFEST" | head -1 | cut -d'"' -f4; }
pin_verify() { # pin_verify <file> <want-sha256> <label>
  local got; got=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $3 ($1): got $got want $2"; exit 1; }
}
WANT_APPL=$(pin_get sha256_appliance_cpio)
WANT_KERN=$(pin_get sha256_l1_kernel)
[ -n "$WANT_APPL" ] && [ -n "$WANT_KERN" ] || { echo "PIN MANIFEST INCOMPLETE: $MANIFEST"; exit 1; }
pin_verify "$BASE/appliance.cpio.gz" "$WANT_APPL" appliance-initrd
pin_verify "/boot/vmlinuz-$KVER" "$WANT_KERN" l1-kernel
cp "$MANIFEST" "$RS/build-manifest.json"
echo "PIN_VERIFIED appliance=$WANT_APPL kernel=$WANT_KERN"

{
  echo "{"
  echo "  \"qemu_sha256\": \"$(sha256sum $QEMU | cut -d' ' -f1)\","
  echo "  \"kernel_sha256\": \"$(sha256sum /boot/vmlinuz-$KVER | cut -d' ' -f1)\","
  echo "  \"initrd_sha256\": \"$(sha256sum $BASE/appliance.cpio.gz | cut -d' ' -f1)\","
  echo "  \"l0_kvm_intel_nested\": \"$(cat /sys/module/kvm_intel/parameters/nested)\","
  echo "  \"l0_kvm_enable_pmu\": \"$(cat /sys/module/kvm/parameters/enable_pmu)\","
  echo "  \"cpuset\": \"$CPUSET\","
  echo "  \"cmdline\": \"q35,accel=kvm -cpu host,pmu=on -smp 1 -m 8192\","
  echo "  \"extra_cmdline\": \"$EXTRA_CMDLINE\","
  echo "  \"timeout_s\": $TIMEOUT,"
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/env.json"

rc=0
# QEMU_EXTRA_ARGS: optional extra QEMU flags (e.g. a QMP socket for the N-3
# pause / live-migration conditions), word-split intentionally.
timeout "$TIMEOUT" taskset -c $CPUSET $QEMU \
    -machine q35,accel=kvm \
    -cpu host,pmu=on \
    -smp 1 -m 8192 \
    -kernel /boot/vmlinuz-$KVER \
    -initrd "$BASE/appliance.cpio.gz" \
    -append "console=ttyS0 rdinit=/init panic=-1 $EXTRA_CMDLINE" \
    -display none -monitor none -no-reboot \
    -pidfile "$RS/qemu.pid" \
    -serial "file:$RS/console.log" \
    ${QEMU_EXTRA_ARGS:-} \
    </dev/null >"$RS/qemu-stdout.log" 2>&1 || rc=$?

echo "qemu_rc=$rc" > "$RS/env.json.rc"

# --- green requires gate RCs, never the done marker alone (PR #98 review
# --- finding 3 / bead hm-b5b item 1): the guest prints NESTED_X86_L1_DONE even
# --- after recording gate failures, so success is: run completed AND at least
# --- one gate ran AND no gate was missing AND every gate that began reported
# --- rc=0. Anything else is a distinct, loud failure.
C="$RS/console.log"
grep -q "NESTED_X86_L1_DONE" "$C" || { echo "RUN_INCOMPLETE $RS (no L1_DONE)"; exit 1; }
began=$(grep -c "NESTED_X86_GATE_BEGIN" "$C" || true)
rcs=$(grep -c "NESTED_X86_GATE_RC " "$C" || true)
fails=$(grep -c "NESTED_X86_GATE_RC .* rc=[1-9]" "$C" || true)
missing=$(grep -c "NESTED_X86_GATE_MISSING" "$C" || true)
[ "$began" -gt 0 ] || { echo "RUN_NO_GATES $RS"; exit 1; }
[ "$missing" -eq 0 ] || { echo "RUN_GATE_MISSING $RS"; grep "NESTED_X86_GATE_MISSING" "$C"; exit 1; }
if [ "$rcs" -ne "$began" ] || [ "$fails" -ne 0 ]; then
  echo "RUN_GATES_FAILED $RS (began=$began rc_lines=$rcs failing=$fails)"
  grep "NESTED_X86_GATE_RC" "$C" || true
  exit 1
fi
echo "RUN_OK $RS ($began/$began gates rc=0)"
