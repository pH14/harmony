#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
RS=/root/nested-x86-spike/n3/results/metal-reference-001
cd /root/harmony-nested
PATCHED=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm
rmmod kvm_intel kvm
insmod $PATCHED/kvm.ko && insmod $PATCHED/kvm-intel.ko
{ echo "=== METAL_GATE_BEGIN live_postgres_rerun $(date -u +%T)"
  timeout 1800 taskset -c 3 ./target/debug/deps/live_postgres-fffcc25f8e2f6c0b --ignored --nocapture --test-threads=1
  echo "=== METAL_GATE_RC live_postgres_rerun rc=$? $(date -u +%T)"
} >> "$RS/console.log" 2>&1 || true
rmmod kvm_intel kvm
modprobe kvm_intel
lsmod | grep "^kvm "
echo METAL_PG_RERUN_DONE
