#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned AA-5(c) arm64 kernel natively on Linux/aarch64 (msr1).
# Publication is fail-closed behind the zero-live-counter and zero-LL/SC scans.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc make flex bison bc xz gzip patch objdump python3

# The canonical M1 build remains the default. M2's std/TetaNES payload needs a
# separate kernel profile with userspace/proc/devmem facilities; keeping its
# object tree and output distinct preserves the sealed M1 artifact byte-for-byte.
arm64_profile=${ARM64_KERNEL_PROFILE:-minimal}
case "$arm64_profile" in
    minimal)
        arm64_source_root=$BUILD_ROOT/arm64-src
        arm64_object_root=$ARM64_KOBJ
        arm64_output=Image
        arm64_extra_fragment=
        ;;
    game)
        arm64_source_root=$BUILD_ROOT/arm64-game-src
        arm64_object_root=$BUILD_ROOT/kernel-build-arm64-game
        arm64_output=Image-game
        arm64_extra_fragment=$LINUX_DIR/arm64-game-config-fragment
        ;;
    postgres)
        arm64_source_root=$BUILD_ROOT/arm64-postgres-src
        arm64_object_root=$BUILD_ROOT/kernel-build-arm64-postgres
        arm64_output=Image-postgres
        arm64_extra_fragment=$LINUX_DIR/arm64-postgres-config-fragment
        ;;
    *)
        echo "FAIL: unknown ARM64_KERNEL_PROFILE=$arm64_profile (want minimal, game, or postgres)" >&2
        exit 1
        ;;
esac

# The arm64 patch stack overlaps itself (0003/0004 modify files 0002 creates), so a
# per-patch "already applied?" probe cannot certify a previously patched tree — and the
# x86 recipe patches the shared $KSRC extract with its own stack. Build from a dedicated
# tree re-extracted pristine on every run, and rebuild the object dir with it. The arm64
# series lives under patches/arm64/ (the x86 series under patches/x86/), so the two
# arches never share a patch number or an applier glob (hm-0dst, tribunal F7).
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
rm -rf "$arm64_source_root" "$arm64_object_root"
mkdir -p "$arm64_source_root"
tar -xf "$kernel_tarball" -C "$arm64_source_root"
KSRC=$arm64_source_root/linux-$KERNEL_VERSION

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
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0005-arm64-harmony-pvclock-from-dt.patch" \
    "harmony DT-discovered pvclock page"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0006-arm64-harmony-lse-only-futex.patch" \
    "harmony LSE-only futex atomics"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0007-arm64-harmony-syscall-tick.patch" \
    "harmony prescriptive syscall tick"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0008-arm64-harmony-fixed-counter-frequency.patch" \
    "harmony fixed counter frequency"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0009-arm64-harmony-fixed-cache-topology.patch" \
    "harmony fixed cache topology"
apply_kernel_patch \
    "$LINUX_DIR/patches/arm64/0010-arm64-harmony-irq-unmask-tick.patch" \
    "harmony IRQ-unmask tick"

mkdir -p "$arm64_object_root" "$ARM64_ART_DIR"

echo "== arm64 kernel: $arm64_profile profile + AA-5(c) determinism overlay (linux-$KERNEL_VERSION)"
make -C "$KSRC" O="$arm64_object_root" ARCH=arm64 tinyconfig
if [ -n "$arm64_extra_fragment" ]; then
    (cd "$KSRC" && ./scripts/kconfig/merge_config.sh -m -O "$arm64_object_root" \
        "$arm64_object_root/.config" "$LINUX_DIR/arm64-config-fragment" \
        "$arm64_extra_fragment")
else
    (cd "$KSRC" && ./scripts/kconfig/merge_config.sh -m -O "$arm64_object_root" \
        "$arm64_object_root/.config" "$LINUX_DIR/arm64-config-fragment")
fi
make -C "$KSRC" O="$arm64_object_root" ARCH=arm64 olddefconfig

assert_y() {
    for sym in "$@"; do
        if ! grep -qxF "CONFIG_$sym=y" "$arm64_object_root/.config"; then
            echo "FAIL: CONFIG_$sym=y did not survive arm64 merge_config/olddefconfig" >&2
            exit 1
        fi
    done
}
assert_off() {
    for sym in "$@"; do
        if grep -q "^CONFIG_$sym=" "$arm64_object_root/.config"; then
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
    PERF_EVENTS HW_PERF_EVENTS
case "$arm64_profile" in
    minimal)
        assert_off BINFMT_SCRIPT PROC_FS FUTEX DEVMEM
        ;;
    game)
        assert_y BINFMT_SCRIPT PROC_FS PROC_PAGE_MONITOR FUTEX DEVMEM MMU SHMEM TMPFS
        assert_off STRICT_DEVMEM
        ;;
    postgres)
        assert_y BINFMT_SCRIPT PROC_FS FUTEX MMU SHMEM TMPFS FILE_LOCKING MULTIUSER \
            SYSVIPC POSIX_MQUEUE NAMESPACES UTS_NS IPC_NS PID_NS NET_NS NET UNIX \
            INET CGROUPS EPOLL EVENTFD SIGNALFD TIMERFD INOTIFY_USER SECCOMP \
            DEVMEM
        assert_off STRICT_DEVMEM
        ;;
esac
if ! grep -qxF 'CONFIG_NR_CPUS=2' "$arm64_object_root/.config"; then
    echo "FAIL: CONFIG_NR_CPUS must be the arm64 minimum (2)" >&2
    exit 1
fi
if ! grep -qxF 'CONFIG_LOCALVERSION=""' "$arm64_object_root/.config"; then
    echo "FAIL: CONFIG_LOCALVERSION must be empty in the AA-5(c) kernel" >&2
    exit 1
fi

echo "== arm64 kernel: building Image + vmlinux"
make -C "$KSRC" O="$arm64_object_root" ARCH=arm64 LOCALVERSION= -j"$(nproc)" Image

# ARM has no generic-timer-register trap on the reachable execution target.
# Unlike x86's reviewed allowlist, one reachable CNTFRQ/CNTVCT/CNTPCT opcode is
# a determinism hole: CNTFRQ differs across the supported substrates, while the
# count registers also vary with host execution.
# The canonical Image is therefore published only after the empty-allowlist
# scanner accepts the symbolized vmlinux.
echo "== arm64 kernel: zero-live-counter reachability gate"
scan=$GUEST_DIR/scripts/aa5-counter-scan.py
scan_probe=$BUILD_ROOT/aa5-counter-scan-probe.S
scan_probe_elf=$BUILD_ROOT/aa5-counter-scan-probe
scan_probe_log=$BUILD_ROOT/aa5-counter-scan-probe.log
cat >"$scan_probe" <<'EOF'
.text
.global _start
_start:
	mrs x0, cntfrq_el0
	mrs x1, cntvct_el0
	mrs x2, revidr_el1
	mrs x3, aidr_el1
	.word 0xd51be340 // executable data mapping: msr cntv_cval_el0, x0
	ret
EOF
cc -nostdlib -static -Wl,-e,_start -o "$scan_probe_elf" "$scan_probe"
if python3 "$scan" "$scan_probe_elf" >"$scan_probe_log" 2>&1; then
    echo "FAIL: AA-5 counter scanner accepted the planted CNTVCT_EL0 probe" >&2
    exit 1
fi
if ! grep -q '^\[REJECT\].*4 host-dependent register read' "$scan_probe_log"; then
    echo "FAIL: AA-5 scanner did not identify all four planted host-dependent reads" >&2
    cat "$scan_probe_log" >&2
    exit 1
fi
if ! grep -q '^\[REJECT\].*1 live-domain timer program' "$scan_probe_log"; then
    echo "FAIL: AA-5 counter scanner did not identify the planted CNTV_CVAL program" >&2
    cat "$scan_probe_log" >&2
    exit 1
fi
echo "ok: scanner rejected the planted live-counter probe"
python3 "$GUEST_DIR/scripts/aa5-counter-scan.py" \
    "$arm64_object_root/vmlinux" "$arm64_object_root/arch/arm64/kernel/vdso/vdso.so.dbg"

# LL/SC changes the retired-branch clock when STXR fails spuriously and
# livelocks under the exact-landing single-step path. The config removes the
# known fallback bodies; this raw executable-word scan is the fail-closed
# artifact proof. Its planted negative control prevents a vacuous green gate.
echo "== arm64 kernel: zero-LL/SC executable-image gate"
exclusive_scan=$GUEST_DIR/scripts/aa4-exclusive-scan.py
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
    "$arm64_object_root/vmlinux" "$arm64_object_root/arch/arm64/kernel/vdso/vdso.so.dbg"

install -m 0644 "$arm64_object_root/arch/arm64/boot/Image" "$ARM64_ART_DIR/$arm64_output"
echo "ok: $ARM64_ART_DIR/$arm64_output"
