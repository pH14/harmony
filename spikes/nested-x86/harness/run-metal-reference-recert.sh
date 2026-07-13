#!/bin/bash
# nested-x86 re-certification: the metal reference session (bead hm-jpu,
# portability gate + the N-2 metal control). COMMITTED — the original
# metal-reference harness lived only on the box (audit provenance gap).
#
# Swaps L0 to the PATCHED kvm modules for the session (recorded), runs the
# gates bare-metal with per-gate RC capture and an RC-checked verdict (no
# silent reruns — a failed gate fails the session), restores the stock
# modules (+ nested default) and verifies the restored posture.
#
# Usage: run-metal-reference-recert.sh [runset-name] [reps] [hammer-deadlines] [hammer-seed]
set -uo pipefail

RS_NAME="${1:-metal-reference-recert-001}"
REPS="${2:-1000}"
N2N="${3:-10000}"
N2SEED="${4:-2600001099}"
RS=/root/nested-x86-spike/n3/results/$RS_NAME
KVER=6.12.90+deb13.1-amd64
PATCHED=/root/kvm-spike/deb612/hdr/usr/src/linux-headers-$KVER/arch/x86/kvm
# same patched-module pins as the appliance build (build-appliance.sh)
PIN_KVM_KO=ce998d6aeb1e9aa694368061e023d1db5e658333c117c405aed212462c543452
PIN_KVM_INTEL_KO=b6e6d3d2c4fd6f08a67ce00d39d9a735219625e5bca4e33a572ce943da13ed2e
mkdir -p "$RS"
cd /root/harmony-nested

pin() { # pin <file> <sha256>
  local got; got=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $1: got $got want $2"; exit 1; }
}
pin "$PATCHED/kvm.ko" "$PIN_KVM_KO"
pin "$PATCHED/kvm-intel.ko" "$PIN_KVM_INTEL_KO"

if pgrep -x qemu-system-x86 >/dev/null 2>&1; then
  echo "QEMU still running — refusing to swap L0 modules"; exit 1
fi

BINS=$(cat /root/nested-x86-recert/gate-bins.txt)
hammer=$(echo "$BINS" | grep n2_nested_hammer)
repeat=$(echo "$BINS" | grep n3_repeat_gate)
det=$(echo "$BINS" | grep live_determinism)

rmmod kvm_intel kvm
insmod "$PATCHED/kvm.ko" && insmod "$PATCHED/kvm-intel.ko"
{
  echo "{"
  echo "  \"posture\": \"BARE METAL patched modules (recorded L0 swap)\","
  echo "  \"source\": \"$(cat .spike-source-commit)\","
  echo "  \"kvm_ko_sha256\": \"$PIN_KVM_KO\","
  echo "  \"kvm_intel_ko_sha256\": \"$PIN_KVM_INTEL_KO\","
  echo "  \"reps\": $REPS, \"hammer_deadlines\": $N2N, \"hammer_seed\": $N2SEED,"
  echo "  \"started\": \"$(date -u +%FT%TZ)\""
  echo "}"
} > "$RS/env.json"

run() { # run <name> <cmd...>
  local name=$1; shift
  echo "=== METAL_GATE_BEGIN $name $(date -u +%T)"
  timeout 21600 taskset -c 3 "$@" 2>&1
  echo "=== METAL_GATE_RC $name rc=$? $(date -u +%T)"
}
{
  run n3_repeat_gate env N3_REPS="$REPS" N3_ITEM=insn-rng "$repeat" --ignored --nocapture --test-threads=1
  run n2_hammer env N2_DEADLINES="$N2N" N2_SEED="$N2SEED" "$hammer" --ignored --nocapture --test-threads=1
  run live_determinism "$det" --ignored --nocapture --test-threads=1
} > "$RS/console.log" 2>&1 || true

# restore stock L0 + verify (the box's kvm_intel defaults to nested=Y)
rmmod kvm_intel kvm
modprobe kvm_intel
{
  lsmod | grep -E "^kvm"
  echo "nested=$(cat /sys/module/kvm_intel/parameters/nested)"
  echo "enable_pmu=$(cat /sys/module/kvm/parameters/enable_pmu)"
} > "$RS/env.json.restore"
STOCK_SIZE=$(lsmod | awk '$1=="kvm"{print $2}')
NESTED=$(cat /sys/module/kvm_intel/parameters/nested)
if [ "$STOCK_SIZE" != 1396736 ] || [ "$NESTED" != Y ]; then
  echo "METAL_RESTORE_FAILED size=$STOCK_SIZE nested=$NESTED"; exit 1
fi

# RC-checked verdict: every gate that began must have reported rc=0
began=$(grep -c "METAL_GATE_BEGIN" "$RS/console.log" || true)
fails=$(grep -c "METAL_GATE_RC .* rc=[1-9]" "$RS/console.log" || true)
rcs=$(grep -c "METAL_GATE_RC" "$RS/console.log" || true)
echo "{\"gates_began\": $began, \"gate_rc_lines\": $rcs, \"gates_failed\": $fails, \"finished\": \"$(date -u +%FT%TZ)\"}" > "$RS/condition-end.json"
if [ "$began" -eq 0 ] || [ "$rcs" -ne "$began" ] || [ "$fails" -ne 0 ]; then
  echo "METAL_REFERENCE_FAILED began=$began rcs=$rcs fails=$fails"; exit 1
fi
echo "METAL_REFERENCE_OK ($began/$began gates rc=0, L0 restored stock nested=Y)"
