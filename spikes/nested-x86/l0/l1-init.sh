#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86 N-0: init for the minimal L1 probe guest.
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null

echo "NESTED_X86_L1_BOOT_OK"

insmod /mod/msr.ko          && echo "msr: loaded"       || echo "msr: FAILED"
insmod /mod/irqbypass.ko    && echo "irqbypass: loaded" || echo "irqbypass: FAILED"
insmod /mod/kvm.ko          && echo "kvm: loaded"       || echo "kvm: FAILED"
insmod /mod/kvm-intel.ko    && echo "kvm-intel: loaded" || echo "kvm-intel: FAILED"

[ -e /dev/cpu/0/msr ] || { mknod -m 600 /dev/cpu/0/msr c 202 0 2>/dev/null || true; }
ls /dev/kvm >/dev/null 2>&1 && echo "L1_DEV_KVM_PRESENT" || echo "L1_DEV_KVM_ABSENT"
cat /sys/module/kvm_intel/parameters/nested 2>/dev/null | grep -q Y \
    && echo "L1_NESTED_PARAM_Y" || echo "L1_NESTED_PARAM_NOT_Y"

# round-6 P1: the probe's own exit status is CAPTURED and emitted — the END
# sentinel alone never implied the probe succeeded (fail-closed lives in
# run-l1-probe.sh, which requires PROBE_RC rc=0 + validated JSON).
echo "NESTED_X86_PROBE_BEGIN"
/probe
probe_rc=$?
echo "NESTED_X86_PROBE_END"
echo "NESTED_X86_PROBE_RC rc=$probe_rc"

echo "--- L1 dmesg (kvm/vmx/pmu) ---"
dmesg | grep -iE "kvm|vmx|pmu|perf" | tail -30
echo "NESTED_X86_L1_DONE"
poweroff -f
