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

# hash-verify the probe image + kernel against the build manifest BEFORE boot
# (PR #98 round-5 P2: recording hashes at launch is not pinning — a post-build
# image swap would otherwise produce valid-looking evidence), and retain the
# manifest with the runset.
MANIFEST="$BASE/build-manifest.json"
[ -f "$MANIFEST" ] || { echo "PIN MANIFEST MISSING: $MANIFEST (run build-l1-probe.sh first)"; exit 1; }
pin_get() { grep -o "\"$1\": \"[0-9a-f]*\"" "$MANIFEST" | head -1 | cut -d'"' -f4; }
pin_verify() { # pin_verify <file> <want> <label>
  local got; got=$(sha256sum "$1" | cut -d' ' -f1)
  [ "$got" = "$2" ] || { echo "PIN MISMATCH $3 ($1): got $got want $2"; exit 1; }
}
WANT_INITRD=$(pin_get "sha256_l1-probe.cpio.gz")
WANT_KERN=$(pin_get "sha256_vmlinuz-$KVER")
[ -n "$WANT_INITRD" ] && [ -n "$WANT_KERN" ] || { echo "PIN MANIFEST INCOMPLETE: $MANIFEST"; exit 1; }
pin_verify "$BASE/l1-probe.cpio.gz" "$WANT_INITRD" probe-initrd
pin_verify "/boot/vmlinuz-$KVER" "$WANT_KERN" l1-kernel
cp "$MANIFEST" "$RS/build-manifest.json"
echo "PIN_VERIFIED initrd=$WANT_INITRD kernel=$WANT_KERN"

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

# fail-closed verdict (PR #98 round-5 P1): l1-init.sh prints the done marker
# even after module/probe failures (the retained runset-001 demonstrates it:
# 'kvm: FAILED' followed by L1_DONE). Green requires qemu rc=0 AND the run
# completing AND zero FAILED module markers AND /dev/kvm present at L1 AND a
# complete probe sentinel pair.
C="$RS/console.log"
fails=$(grep -c ": FAILED" "$C" || true)
kvm_present=$(grep -c "L1_DEV_KVM_PRESENT" "$C" || true)
pb=$(grep -c "NESTED_X86_PROBE_BEGIN" "$C" || true)
pe=$(grep -c "NESTED_X86_PROBE_END" "$C" || true)
grep -q "NESTED_X86_L1_DONE" "$C" || { echo "RUN_INCOMPLETE $RS (no L1_DONE)"; exit 1; }
if [ "$rc" -ne 0 ] || [ "$fails" -ne 0 ] || [ "$kvm_present" -lt 1 ] \
   || [ "$pb" -lt 1 ] || [ "$pe" -lt 1 ]; then
  echo "RUN_PROBE_FAILED $RS (qemu_rc=$rc failed_markers=$fails kvm_present=$kvm_present probe=$pb/$pe)"
  grep ": FAILED\|L1_DEV_KVM" "$C" || true
  exit 1
fi
echo "RUN_OK $RS (modules clean, /dev/kvm present, probe complete)"
