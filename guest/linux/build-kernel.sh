#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned guest kernel: a vendored **Kata Containers guest-kernel config**
# (kata/, the container-host base) + the **determinism overlay** (config-fragment),
# merged on top so it wins, out-of-tree at a fixed O= path, with all reproducibility
# levers set (see lib-build.sh). Task 36 rebased the base from `tinyconfig` to Kata's
# config; the overlay is unchanged in intent. See guest/linux/IMPLEMENTATION.md.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make flex bison bc xz gzip
extract_kernel

# Task 110: apply the harmony guest-kernel source additions (the
# CONFIG_HARMONY_PVCLOCK clocksource — the guest half of the paravirt
# work-derived clock, docs/PARAVIRT-CLOCK.md §3.1). String-anchored and
# idempotent; aborts loudly on a drifted tree (see patches/).
python3 "$LINUX_DIR/patches/apply-guest-patches.py" "$KSRC"

mkdir -p "$KOBJ" "$ART_DIR"

# Base = Kata guest-kernel config. Kata builds from `allnoconfig` + its fragments
# (its build-kernel.sh passes merge_config.sh -n), so seed with allnoconfig — NOT
# tinyconfig (whose tiny.config size-optimization deltas are not part of Kata's
# config). Then merge, in one pass, the Kata fragments (the container-host base)
# followed by config-fragment (the determinism overlay) LAST so it overrides every
# conflict (SMP/NUMA/KASLR/HZ/HW_RANDOM/... -> off). olddefconfig resolves deps.
echo "== kernel: Kata guest-kernel config (kata/, pinned $(sed -n 's/^- *Release: *//p' kata/PROVENANCE | head -1)) + determinism overlay (linux-$KERNEL_VERSION)"
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 allnoconfig
(cd "$KSRC" && ./scripts/kconfig/merge_config.sh -m -O "$KOBJ" \
    "$KOBJ/.config" \
    "$LINUX_DIR"/kata/common/*.conf \
    "$LINUX_DIR"/kata/x86_64/*.conf \
    "$LINUX_DIR/config-fragment")
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 olddefconfig

# merge_config only warns when a fragment symbol cannot take effect; assert the ones
# the image cannot work without (or that determinism cannot lose). Crucially, every
# determinism symbol in the overlay must SURVIVE the merge against the *richer* Kata
# base, which sets several of them the other way (SMP=y, NO_HZ_FULL=y, CPU_FREQ=y,
# RANDOMIZE_BASE=y, X86_PM_TIMER=y, HW_RANDOM=y, ...). Assert each, loudly.
assert_y() {
    for sym in "$@"; do
        if ! grep -qxF "CONFIG_$sym=y" "$KOBJ/.config"; then
            echo "FAIL: CONFIG_$sym=y did not survive merge_config/olddefconfig" >&2
            exit 1
        fi
    done
}
assert_off() {
    for sym in "$@"; do
        if grep -q "^CONFIG_$sym=" "$KOBJ/.config"; then
            echo "FAIL: CONFIG_$sym is enabled but must be off" >&2
            exit 1
        fi
    done
}
# Functional must-haves for the boot-to-/init image (provided by Kata and/or overlay).
# HARMONY_PVCLOCK (task 110) is compiled in but runtime-inert without the
# harmony_pvclock kernel parameter, so one image serves as both the page-on
# and page-off measurement arm.
assert_y 64BIT PRINTK TTY SERIAL_8250 SERIAL_8250_CONSOLE BINFMT_ELF \
    BINFMT_SCRIPT BLK_DEV_INITRD RD_GZIP PROC_FS SYSFS DEVTMPFS ACPI PCI \
    HZ_PERIODIC HZ_100 FUTEX POSIX_TIMERS KERNEL_GZIP X86_IOPL_IOPERM DEVMEM \
    HARMONY_PVCLOCK
# (HPET_TIMER is not in this list: it is def_bool y on x86-64 with no prompt;
# the HPET is excluded at runtime instead — see config-fragment.)
# Determinism overlay: every symbol below is set ON by the Kata base and must be
# flipped OFF by config-fragment (or is absent because the overlay won the timer
# choice). EXT4_FS is deliberately NOT here any more — the container workload
# (tasks 37/38) needs it, and Kata provides it; see the capability audit.
# Dynticks: assert the *meaningful* tickless symbols off — NO_HZ_COMMON (selects
# the dynticks machinery + TICK_ONESHOT) and the choice members NO_HZ_FULL/
# NO_HZ_IDLE. NOT plain CONFIG_NO_HZ: that is the deprecated "Old Idle dynticks
# config" bool which only sets the *default* of the "Timer tick handling" choice
# ("default NO_HZ_IDLE if NO_HZ"); Kata sets it =y, but once HZ_PERIODIC wins the
# choice it is inert (it selects nothing), so it harmlessly stays =y.
assert_off NUMA CPU_FREQ MODULES TRANSPARENT_HUGEPAGE KSM SUSPEND \
    HIBERNATION X86_PM_TIMER HIGH_RES_TIMERS RANDOMIZE_BASE \
    LOCALVERSION_AUTO HW_RANDOM NO_HZ_COMMON NO_HZ_FULL NO_HZ_IDLE TICK_ONESHOT
# Empty version suffix: git/build state must not leak into the bytes.
if ! grep -qxF 'CONFIG_LOCALVERSION=""' "$KOBJ/.config"; then
    echo "FAIL: CONFIG_LOCALVERSION must be empty (reproducibility)" >&2
    exit 1
fi

echo "== kernel: building bzImage"
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 LOCALVERSION= -j"$(nproc)" bzImage

install -m 0644 "$KOBJ/arch/x86/boot/bzImage" "$ART_DIR/bzImage"
echo "ok: $ART_DIR/bzImage"

# Task 110: the counter-opcode reachability gate (PARAVIRT-CLOCK.md §3.3, x86
# half) — every rdtsc/rdtscp left in the image must match a reviewed,
# trap-backstopped allowlist entry (function + exact instruction count). Scans
# the uncompressed vmlinux (symbols); self-tests its own ability to fail
# before scanning. While rdtsc-allowlist.txt carries its GATE-UNARMED marker
# (baseline pending) the scan runs in capture mode — prints the found sites
# and exits 0 — so this build works before the baseline review; removing the
# marker arms the gate. See scan-counter-opcodes.sh for the full workflow.
echo "== kernel: counter-opcode scan (rdtsc/rdtscp reachability gate)"
bash "$LINUX_DIR/scan-counter-opcodes.sh" "$KOBJ/vmlinux" "$LINUX_DIR/rdtsc-allowlist.txt"
