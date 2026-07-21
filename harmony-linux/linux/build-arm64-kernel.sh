#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned AA-5(c) arm64 kernel natively on the pinned Altra box.
# Publication is fail-closed behind the zero-live-counter and zero-LL/SC scans.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make flex bison bc xz gzip patch objdump python3

# The arm64 patch stack overlaps itself (0003/0004 modify files 0002 creates), so a
# per-patch "already applied?" probe cannot certify a previously patched tree — and the
# x86 recipe patches the shared $KSRC extract with its own stack. Build from a dedicated
# tree re-extracted pristine on every run, and rebuild the object dir with it. The arm64
# series lives under patches/arm64/ (the x86 series under patches/x86/), so the two
# arches never share a patch number or an applier glob (hm-0dst, tribunal F7).
ARM64_SRC_ROOT=$BUILD_ROOT/arm64-src
kernel_tarball=$DL_DIR/$(basename "$KERNEL_URL")
if [ ! -f "$kernel_tarball" ]; then
    echo "FAIL: $kernel_tarball missing — run 'make -C harmony-linux fetch' first (needs network once)" >&2
    exit 1
fi
got=$(sha256_of "$kernel_tarball")
if [ "$got" != "$KERNEL_SHA256" ]; then
    echo "FAIL: $kernel_tarball sha256 mismatch (want $KERNEL_SHA256, got $got)" >&2
    exit 1
fi
echo "== arm64 kernel: pristine extract of linux-$KERNEL_VERSION (sha256 verified)"
rm -rf "$ARM64_SRC_ROOT" "$ARM64_KOBJ"
mkdir -p "$ARM64_SRC_ROOT"
tar -xf "$kernel_tarball" -C "$ARM64_SRC_ROOT"
KSRC=$ARM64_SRC_ROOT/linux-$KERNEL_VERSION

apply_kernel_patch() {
    patch_file=$1
    patch_label=$2
    if [ ! -f "$patch_file" ]; then
        echo "FAIL: required $patch_label kernel patch is missing: $patch_file" >&2
        exit 1
    fi
    if ! (cd "$KSRC" && patch -p1 --dry-run --force <"$patch_file") >/dev/null 2>&1; then
        echo "FAIL: $patch_label patch does not apply cleanly to the pristine tree" >&2
        exit 1
    fi
    echo "== arm64 kernel: applying $patch_label patch"
    (cd "$KSRC" && patch -p1 --force <"$patch_file")
}

apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0002-arm64-harmony-pvclock-work-derived-clocksource.patch" \
    "harmony pvclock"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0003-arm64-harmony-lse-only.patch" \
    "harmony LSE-only"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0004-arm64-harmony-work-clockevent.patch" \
    "harmony work clockevent"

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
    SERIAL_AMBA_PL011_CONSOLE BINFMT_ELF BLK_DEV_INITRD \
    RD_GZIP SYSFS DEVTMPFS POSIX_TIMERS ARM_ARCH_TIMER \
    ARM_PSCI_FW IRQCHIP ARM_GIC ARM_GIC_V3 HARMONY_ARM_PVCLOCK \
    GENERIC_IDLE_POLL_SETUP \
    ARM64_USE_LSE_ATOMICS ARM64_LSE_ATOMICS HARMONY_ARM_LSE_ONLY \
    HZ_PERIODIC HZ_100 STRICT_KERNEL_RWX
assert_off HOTPLUG_CPU CPU_FREQ CPU_IDLE MODULES HIGH_RES_TIMERS NO_HZ_COMMON \
    NO_HZ_IDLE NO_HZ_FULL RANDOMIZE_BASE HW_RANDOM \
    TRANSPARENT_HUGEPAGE KSM SUSPEND HIBERNATION \
    ARM_ARCH_TIMER_EVTSTREAM ARM_ARCH_TIMER_OOL_WORKAROUND \
    FSL_ERRATUM_A008585 HISILICON_ERRATUM_161010101 \
    ARM64_ERRATUM_858921 SUN50I_ERRATUM_UNKNOWN1 KVM COMPAT ACPI \
    BPF_SYSCALL BPF_JIT KPROBES FUNCTION_TRACER FTRACE LIVEPATCH \
    PERF_EVENTS HW_PERF_EVENTS BINFMT_SCRIPT PROC_FS FUTEX
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
	.word 0xd51be340 // executable data mapping: msr cntv_cval_el0, x0
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
if ! grep -q '^\[REJECT\].*1 live-domain timer program' "$scan_probe_log"; then
    echo "FAIL: AA-5 counter scanner did not identify the planted CNTV_CVAL program" >&2
    cat "$scan_probe_log" >&2
    exit 1
fi
echo "ok: scanner rejected the planted live-counter probe"
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa5-counter-scan.py" \
    "$ARM64_KOBJ/vmlinux" "$ARM64_KOBJ/arch/arm64/kernel/vdso/vdso.so.dbg"

# LL/SC changes the retired-branch clock when STXR fails spuriously and
# livelocks under the exact-landing single-step path. The config removes the
# known fallback bodies; this raw executable-word scan is the fail-closed
# artifact proof. Its planted negative control prevents a vacuous green gate.
echo "== arm64 kernel: zero-LL/SC executable-image gate"
exclusive_scan=$GUEST_DIR/../spikes/arm-altra/host/aa4-exclusive-scan.py
exclusive_probe=$BUILD_ROOT/aa4-exclusive-scan-probe.S
exclusive_probe_elf=$BUILD_ROOT/aa4-exclusive-scan-probe
exclusive_probe_log=$BUILD_ROOT/aa4-exclusive-scan-probe.log
cat >"$exclusive_probe" <<'EOF'
.text
.global _start
_start:
	.word 0x885f7c20 // executable data mapping: ldxr w0, [x1]
	.inst 0x88027c20 // stxr w2, w0, [x1]
	ret
EOF
cc -nostdlib -static -Wl,-e,_start -o "$exclusive_probe_elf" "$exclusive_probe"
if python3 "$exclusive_scan" "$exclusive_probe_elf" >"$exclusive_probe_log" 2>&1; then
    echo "FAIL: AA-4 exclusive scanner accepted the planted LDXR/STXR probe" >&2
    exit 1
fi
if ! grep -q '^\[BANNED\].*: 2 LL/SC exclusive instruction(s)$' "$exclusive_probe_log"; then
    echo "FAIL: AA-4 exclusive scanner did not identify exactly two planted exclusives" >&2
    cat "$exclusive_probe_log" >&2
    exit 1
fi
echo "ok: scanner rejected the planted LDXR/STXR probe"
python3 "$exclusive_scan" \
    "$ARM64_KOBJ/vmlinux" "$ARM64_KOBJ/arch/arm64/kernel/vdso/vdso.so.dbg"

install -m 0644 "$ARM64_KOBJ/arch/arm64/boot/Image" "$ARM64_ART_DIR/Image"
echo "ok: $ARM64_ART_DIR/Image"
