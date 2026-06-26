#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the pinned guest kernel: tinyconfig + config-fragment, out-of-tree at
# a fixed O= path, with all reproducibility levers set (see lib-build.sh).
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make flex bison bc xz gzip
extract_kernel

mkdir -p "$KOBJ" "$ART_DIR"

echo "== kernel: tinyconfig + config-fragment (linux-$KERNEL_VERSION)"
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 tinyconfig
(cd "$KSRC" && ./scripts/kconfig/merge_config.sh -m -O "$KOBJ" \
    "$KOBJ/.config" "$LINUX_DIR/config-fragment")
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 olddefconfig

# merge_config only warns when a fragment symbol cannot take effect; assert
# the ones the image cannot work without (or that determinism cannot lose).
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
assert_y 64BIT PRINTK TTY SERIAL_8250 SERIAL_8250_CONSOLE BINFMT_ELF \
    BINFMT_SCRIPT BLK_DEV_INITRD RD_GZIP PROC_FS SYSFS DEVTMPFS ACPI PCI \
    HZ_PERIODIC HZ_100 FUTEX POSIX_TIMERS KERNEL_GZIP
# (HPET_TIMER is not in this list: it is def_bool y on x86-64 with no prompt;
# the HPET is excluded at runtime instead — see config-fragment.)
assert_off SMP NUMA CPU_FREQ MODULES TRANSPARENT_HUGEPAGE KSM SUSPEND \
    HIBERNATION X86_PM_TIMER HIGH_RES_TIMERS RANDOMIZE_BASE \
    LOCALVERSION_AUTO EXT4_FS HW_RANDOM

echo "== kernel: building bzImage"
make -C "$KSRC" O="$KOBJ" ARCH=x86_64 LOCALVERSION= -j"$(nproc)" bzImage

install -m 0644 "$KOBJ/arch/x86/boot/bzImage" "$ART_DIR/bzImage"
echo "ok: $ART_DIR/bzImage"
