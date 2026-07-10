#!/bin/sh
# nested-x86 N-1: init for the consonance appliance L1 guest.
# Loads the PATCHED kvm modules, verifies the L2 image pins from inside L1,
# then runs the determinism-ABI gates. All output goes to the serial console.
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null
mount -t tmpfs -o size=512m tmpfs /tmp

echo "NESTED_X86_L1_BOOT_OK"
uname -a
free -m | head -2

insmod /mod/msr.ko          && echo "msr: loaded"       || echo "msr: FAILED"
insmod /mod/irqbypass.ko    && echo "irqbypass: loaded" || echo "irqbypass: FAILED"
insmod /mod/kvm.ko          && echo "kvm(PATCHED): loaded"       || echo "kvm(PATCHED): FAILED"
insmod /mod/kvm-intel.ko    && echo "kvm-intel(PATCHED): loaded" || echo "kvm-intel(PATCHED): FAILED"
ls /dev/kvm >/dev/null 2>&1 && echo "L1_DEV_KVM_PRESENT" || echo "L1_DEV_KVM_ABSENT"

# content-hash-verify the L2 pair from INSIDE L1 before any boot of it
echo "NESTED_X86_L2_PIN_CHECK_BEGIN"
cd /root/harmony-nested
sha256sum guest/build/bzImage guest/build/initramfs-postgres.cpio.gz
echo "NESTED_X86_L2_PIN_CHECK_END"

export RUST_BACKTRACE=1
export TMPDIR=/tmp

run_gate() { # run_gate <name> [args...]
    name=$1; shift
    echo "NESTED_X86_GATE_BEGIN $name"
    /gate/$name --ignored --nocapture --test-threads=1 "$@" 2>&1
    echo "NESTED_X86_GATE_RC $name rc=$?"
    echo "NESTED_X86_GATE_END $name"
}

run_gate live_determinism
run_gate live_preemption
run_gate live_postgres

echo "--- L1 dmesg tail (kvm/vmx/pmu/perf) ---"
dmesg | grep -iE "kvm|vmx|pmu|perf" | tail -30
echo "NESTED_X86_L1_DONE"
poweroff -f
