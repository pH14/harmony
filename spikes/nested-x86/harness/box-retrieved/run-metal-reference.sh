#!/bin/bash
# N-3 portability references + N-4 metal timings: one patched-module session.
set -euo pipefail
RS=/root/nested-x86-spike/n3/results/metal-reference-001
mkdir -p "$RS"
cd /root/harmony-nested
PATCHED=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-6.12.90+deb13.1-amd64/arch/x86/kvm
rmmod kvm_intel kvm
insmod $PATCHED/kvm.ko && insmod $PATCHED/kvm-intel.ko
echo "{\"posture\": \"BARE METAL patched modules\", \"source\": \"$(cat .spike-source-commit)\", \"started\": \"$(date -u +%FT%TZ)\"}" > "$RS/env.json"
run() { name=$1; shift; echo "=== METAL_GATE_BEGIN $name $(date -u +%T)"; timeout 1800 taskset -c 3 "$@" 2>&1; echo "=== METAL_GATE_RC $name rc=$? $(date -u +%T)"; }
{
  run n3_repeat_gate env N3_REPS=100 N3_ITEM=insn-rng ./target/debug/deps/n3_repeat_gate-ee2d3fab6d015a04 --ignored --nocapture --test-threads=1
  run n2_hammer env N2_DEADLINES=10000 ./target/debug/deps/n2_nested_hammer-a7cc3f47b8e93461 --ignored --nocapture --test-threads=1
  run live_determinism ./target/debug/deps/live_determinism-738f21d807cbcac0 --ignored --nocapture --test-threads=1
  run live_preemption ./target/debug/deps/live_preemption-2b1f04ca2476e06f --ignored --nocapture --test-threads=1
  run live_postgres ./target/debug/deps/live_postgres-fffcc25f8e2f6c0b --ignored --nocapture --test-threads=1
} > "$RS/console.log" 2>&1 || true
rmmod kvm_intel kvm
modprobe kvm_intel
{ lsmod | grep "^kvm "; cat /sys/module/kvm_intel/parameters/nested; } >> "$RS/env.json.restore"
echo METAL_REFERENCE_DONE
