#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 N-0: launch the minimal L1 probe under stock-KVM L0. One run-set.
# Usage: run-l1-probe.sh <runset-name>
set -euo pipefail

BASE=/root/nested-x86-spike/n0
KVER=6.12.90+deb13.1-amd64
RS="$BASE/results/${1:?runset name required}"
mkdir -p "$RS"

QEMU=/usr/bin/qemu-system-x86_64
CPUSET=3   # pinned per box core discipline (leased core set)

{
  echo "{"
  echo "  \"qemu_sha256\": \"$(sha256sum $QEMU | cut -d' ' -f1)\","
  echo "  \"kernel_sha256\": \"$(sha256sum /boot/vmlinuz-$KVER | cut -d' ' -f1)\","
  echo "  \"initrd_sha256\": \"$(sha256sum $BASE/l1-probe.cpio.gz | cut -d' ' -f1)\","
  echo "  \"l0_kvm_intel_nested\": \"$(cat /sys/module/kvm_intel/parameters/nested)\","
  echo "  \"l0_kvm_enable_pmu\": \"$(cat /sys/module/kvm/parameters/enable_pmu)\","
  echo "  \"cpuset\": \"$CPUSET\","
  echo "  \"cmdline\": \"q35,accel=kvm -cpu host,pmu=on -smp 1 -m 2048\","
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/env.json"

rc=0
timeout 180 taskset -c $CPUSET $QEMU \
    -machine q35,accel=kvm \
    -cpu host,pmu=on \
    -smp 1 -m 2048 \
    -kernel /boot/vmlinuz-$KVER \
    -initrd "$BASE/l1-probe.cpio.gz" \
    -append "console=ttyS0 rdinit=/init panic=-1" \
    -display none -monitor none -no-reboot \
    -serial "file:$RS/console.log" \
    </dev/null >"$RS/qemu-stdout.log" 2>&1 || rc=$?

echo "qemu_rc=$rc" >> "$RS/env.json.rc"
grep -q "NESTED_X86_L1_DONE" "$RS/console.log" && echo "RUN_OK $RS" || { echo "RUN_INCOMPLETE $RS"; exit 1; }
