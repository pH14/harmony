#!/bin/sh
# nested-x86: init for the consonance appliance L1 guest.
# Loads the PATCHED kvm modules, verifies the L2 image pins from inside L1,
# then runs the gate sequence named on the kernel cmdline:
#   harmony.gates=live_determinism,live_preemption   (default: all baked gates)
#   harmony.env=N2_DEADLINES=250000,N2_SEED=123      (exported before gates run)
# All output goes to the serial console.
export PATH=/bin
mount -t proc proc /proc
mount -t sysfs sys /sys
mount -t devtmpfs dev /dev 2>/dev/null
mount -t tmpfs -o size=512m tmpfs /tmp

echo "NESTED_X86_L1_BOOT_OK"
uname -a
free -m | head -2
cat /proc/cmdline

insmod /mod/msr.ko          && echo "msr: loaded"       || echo "msr: FAILED"
insmod /mod/irqbypass.ko    && echo "irqbypass: loaded" || echo "irqbypass: FAILED"
insmod /mod/kvm.ko          && echo "kvm(PATCHED): loaded"       || echo "kvm(PATCHED): FAILED"
insmod /mod/kvm-intel.ko    && echo "kvm-intel(PATCHED): loaded" || echo "kvm-intel(PATCHED): FAILED"
ls /dev/kvm >/dev/null 2>&1 && echo "L1_DEV_KVM_PRESENT" || echo "L1_DEV_KVM_ABSENT"

# content-hash-verify the L2 pair from INSIDE L1 before any boot of it
echo "NESTED_X86_L2_PIN_CHECK_BEGIN"
cd "$(cat /srcroot 2>/dev/null || echo /root/harmony-nested)"
sha256sum guest/build/bzImage guest/build/initramfs-postgres.cpio.gz
echo "NESTED_X86_L2_PIN_CHECK_END"

export RUST_BACKTRACE=1
export TMPDIR=/tmp

# parse harmony.gates= and harmony.env= from the kernel cmdline
GATES=""
for tok in $(cat /proc/cmdline); do
    case "$tok" in
        harmony.gates=*) GATES=$(echo "${tok#harmony.gates=}" | sed 's/,/ /g') ;;
        harmony.env=*)
            for kv in $(echo "${tok#harmony.env=}" | sed 's/,/ /g'); do
                export "$kv"
                echo "ENV $kv"
            done ;;
    esac
done
[ -n "$GATES" ] || GATES="live_determinism live_preemption live_postgres"

# Gate failures are aggregated and echoed before the done marker so the host
# harness has a redundant machine-readable failure count; NESTED_X86_L1_DONE
# means only "the run completed" — the host-side success check requires every
# NESTED_X86_GATE_RC to be rc=0 (PR #98 review finding 3).
GATE_FAILS=0
run_gate() { # run_gate <name> [test-filter]
    name=$1
    filter=${2:-}
    echo "NESTED_X86_GATE_BEGIN $name $filter"
    /gate/$name --ignored --nocapture --test-threads=1 $filter 2>&1
    gate_rc=$?
    echo "NESTED_X86_GATE_RC $name rc=$gate_rc"
    [ "$gate_rc" -eq 0 ] || GATE_FAILS=$((GATE_FAILS + 1))
    echo "NESTED_X86_GATE_END $name"
}

for g in $GATES; do
    bin=${g%%:*}
    filter=""
    [ "$g" != "$bin" ] && filter=${g#*:}
    if [ -x "/gate/$bin" ]; then
        run_gate "$bin" "$filter"
    else
        echo "NESTED_X86_GATE_MISSING $bin"
        GATE_FAILS=$((GATE_FAILS + 1))
    fi
done

echo "--- L1 dmesg tail (kvm/vmx/pmu/perf) ---"
dmesg | grep -iE "kvm|vmx|pmu|perf" | tail -30
echo "NESTED_X86_GATES_FAILED $GATE_FAILS"
echo "NESTED_X86_L1_DONE"
poweroff -f
