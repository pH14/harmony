#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned AA-5(c) arm64 kernel natively on the pinned Altra box.
# Publication is fail-closed behind the zero-live-counter opcode scan.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make flex bison bc xz gzip patch objdump python3
extract_kernel

PVCLOCK_PATCH=$LINUX_DIR/patches/0002-arm64-harmony-pvclock-work-derived-clocksource.patch
if [ ! -f "$PVCLOCK_PATCH" ]; then
    echo "FAIL: required AA-5(c) kernel patch is missing: $PVCLOCK_PATCH" >&2
    exit 1
fi
if (cd "$KSRC" && patch -p1 -R --dry-run --force <"$PVCLOCK_PATCH") >/dev/null 2>&1; then
    echo "== arm64 kernel: harmony pvclock patch already applied"
else
    echo "== arm64 kernel: applying harmony pvclock patch"
    (cd "$KSRC" && patch -p1 --force <"$PVCLOCK_PATCH")
fi

mkdir -p "$ARM64_KOBJ" "$ARM64_ART_DIR"

echo "== arm64 kernel: tinyconfig + AA-5(c) determinism overlay (linux-$KERNEL_VERSION)"
make -C "$KSRC" O="$ARM64_KOBJ" ARCH=arm64 tinyconfig
(cd "$KSRC" && ./scripts/kconfig/merge_config.sh -m -O "$ARM64_KOBJ" \
    "$ARM64_KOBJ/.config" "$LINUX_DIR/arm64-config-fragment")
make -C "$KSRC" O="$ARM64_KOBJ" ARCH=arm64 olddefconfig

assert_y() {
    for sym in "$@"; do
        if ! grep -qxF "CONFIG_$sym=y" "$ARM64_KOBJ/.config"; then
            echo "FAIL: CONFIG_$sym=y did not survive arm64 merge_config/olddefconfig" >&2
            exit 1
        fi
    done
}
assert_off() {
    for sym in "$@"; do
        if grep -q "^CONFIG_$sym=" "$ARM64_KOBJ/.config"; then
            echo "FAIL: CONFIG_$sym is enabled but must be off in the AA-5(c) image" >&2
            exit 1
        fi
    done
}

assert_y ARM64 64BIT SMP OF PRINTK TTY SERIAL_AMBA_PL011 \
    SERIAL_AMBA_PL011_CONSOLE BINFMT_ELF BINFMT_SCRIPT BLK_DEV_INITRD \
    RD_GZIP PROC_FS SYSFS DEVTMPFS FUTEX POSIX_TIMERS ARM_ARCH_TIMER \
    ARM_PSCI_FW IRQCHIP ARM_GIC ARM_GIC_V3 HARMONY_ARM_PVCLOCK \
    HZ_PERIODIC HZ_100 STRICT_KERNEL_RWX
assert_off HOTPLUG_CPU CPU_FREQ CPU_IDLE MODULES HIGH_RES_TIMERS NO_HZ_COMMON \
    NO_HZ_IDLE NO_HZ_FULL RANDOMIZE_BASE HW_RANDOM \
    TRANSPARENT_HUGEPAGE KSM SUSPEND HIBERNATION \
    ARM_ARCH_TIMER_EVTSTREAM ARM_ARCH_TIMER_OOL_WORKAROUND \
    FSL_ERRATUM_A008585 HISILICON_ERRATUM_161010101 \
    ARM64_ERRATUM_858921 SUN50I_ERRATUM_UNKNOWN1 KVM COMPAT ACPI \
    BPF_SYSCALL BPF_JIT KPROBES FUNCTION_TRACER FTRACE LIVEPATCH \
    PERF_EVENTS HW_PERF_EVENTS
if ! grep -qxF 'CONFIG_NR_CPUS=2' "$ARM64_KOBJ/.config"; then
    echo "FAIL: CONFIG_NR_CPUS must be the arm64 minimum (2)" >&2
    exit 1
fi
if ! grep -qxF 'CONFIG_LOCALVERSION=""' "$ARM64_KOBJ/.config"; then
    echo "FAIL: CONFIG_LOCALVERSION must be empty in the AA-5(c) kernel" >&2
    exit 1
fi

echo "== arm64 kernel: building Image + vmlinux"
make -C "$KSRC" O="$ARM64_KOBJ" ARCH=arm64 LOCALVERSION= -j"$(nproc)" Image

# ARM has no generic-counter trap on the reachable N1 silicon. Unlike x86's
# reviewed allowlist, one reachable CNTVCT/CNTPCT opcode is a determinism hole.
# The canonical Image is therefore published only after the empty-allowlist
# scanner accepts the symbolized vmlinux.
echo "== arm64 kernel: zero-live-counter reachability gate"
scan=$GUEST_DIR/../spikes/arm-altra/host/aa5-counter-scan.py
scan_probe=$BUILD_ROOT/aa5-counter-scan-probe.S
scan_probe_elf=$BUILD_ROOT/aa5-counter-scan-probe
scan_probe_log=$BUILD_ROOT/aa5-counter-scan-probe.log
cat >"$scan_probe" <<'EOF'
.text
.global _start
_start:
	mrs x0, cntfrq_el0
	mrs x1, cntvct_el0
	ret
EOF
cc -nostdlib -static -Wl,-e,_start -o "$scan_probe_elf" "$scan_probe"
if python3 "$scan" "$scan_probe_elf" >"$scan_probe_log" 2>&1; then
    echo "FAIL: AA-5 counter scanner accepted the planted CNTVCT_EL0 probe" >&2
    exit 1
fi
if ! grep -q '^\[REJECT\].*1 live counter read' "$scan_probe_log"; then
    echo "FAIL: AA-5 counter scanner did not identify exactly one planted live read" >&2
    cat "$scan_probe_log" >&2
    exit 1
fi
echo "ok: scanner rejected the planted live-counter probe"
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa5-counter-scan.py" \
    "$ARM64_KOBJ/vmlinux" "$ARM64_KOBJ/arch/arm64/kernel/vdso/vdso.so.dbg"

install -m 0644 "$ARM64_KOBJ/arch/arm64/boot/Image" "$ARM64_ART_DIR/Image"
echo "ok: $ARM64_ART_DIR/Image"
