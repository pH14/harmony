#!/bin/bash
# nested-x86: boot the consonance appliance under stock-KVM L0. One run-set.
# Usage: run-appliance.sh <results-dir> [timeout-seconds] [extra-cmdline]
#   extra-cmdline e.g. "harmony.gates=n2_nested_hammer harmony.env=N2_DEADLINES=250000"
set -euo pipefail

BASE=/root/nested-x86-spike/n1
KVER=6.12.90+deb13.1-amd64
RS="${1:?results dir required}"
case "$RS" in /*) ;; *) RS="$BASE/results/$RS" ;; esac
TIMEOUT="${2:-1800}"
EXTRA_CMDLINE="${3:-}"
mkdir -p "$RS"

QEMU=/usr/bin/qemu-system-x86_64
CPUSET="${CPUSET_OVERRIDE:-3}"   # pinned per box core discipline; override for the migration condition

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
grep -q "NESTED_X86_L1_DONE" "$RS/console.log" && echo "RUN_OK $RS" || { echo "RUN_INCOMPLETE $RS"; exit 1; }
