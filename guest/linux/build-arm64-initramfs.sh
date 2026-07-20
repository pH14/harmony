#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the AA-5(c) arm64 initramfs natively on the pinned Altra box: a
# freestanding syscall-only /init, packed reproducibly with gen_init_cpio.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64
require_tools cc gzip objdump python3
extract_kernel # for usr/gen_init_cpio.c

mkdir -p "$ARM64_ART_DIR"

echo "== arm64 initramfs: building freestanding LSE-only init"
arm64_init=$BUILD_ROOT/arm64-init
arm64_init_cc() {
    cc -Os -Wall -Wextra -Werror -nostdlib -static -ffreestanding -fno-builtin \
        -fno-stack-protector -fno-pie -march=armv8.1-a+lse -mno-outline-atomics \
        -no-pie -Wl,-e,_start -Wl,--build-id=none -Wl,-z,noexecstack \
        "$@" "$LINUX_DIR/arm64-init.c"
}
arm64_init_cc -o "$arm64_init"
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa4-exclusive-scan.py" "$arm64_init"
# The SHIPPED init must be counter-clean (AA-5 closure covers userspace too).
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa5-counter-scan.py" "$arm64_init"

# The el0probe variant carries ONE planted CNTVCT_EL0 read (the AA-5(b) live
# closure probe). It ships only in its own initramfs, never the canonical one,
# and doubles as the counter scanner's per-build negative control.
echo "== arm64 initramfs: building el0probe init variant"
el0probe_init=$BUILD_ROOT/arm64-init-el0probe
el0probe_log=$BUILD_ROOT/arm64-init-el0probe.scan.log
arm64_init_cc -DHARMONY_AA5_EL0_PROBE -o "$el0probe_init"
python3 "$GUEST_DIR/../spikes/arm-altra/host/aa4-exclusive-scan.py" "$el0probe_init"
if python3 "$GUEST_DIR/../spikes/arm-altra/host/aa5-counter-scan.py" "$el0probe_init" >"$el0probe_log" 2>&1; then
    echo "FAIL: AA-5 counter scanner accepted the el0probe init's planted CNTVCT_EL0 read" >&2
    exit 1
fi
if ! grep -q '^\[REJECT\].*1 live counter read' "$el0probe_log"; then
    echo "FAIL: AA-5 counter scanner did not identify exactly one planted live read in el0probe init" >&2
    cat "$el0probe_log" >&2
    exit 1
fi
echo "ok: scanner rejected the el0probe init's single planted live read"

echo "== arm64 initramfs: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio-arm64" "$KSRC/usr/gen_init_cpio.c"

pack_initramfs() {
    init_elf=$1
    out=$2
    spec=$BUILD_ROOT/initramfs-arm64-$(basename "$out" .cpio.gz).spec
    cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
dir /sys 0755 0 0
file /init $init_elf 0755 0 0
EOF
    "$BUILD_ROOT/gen_init_cpio-arm64" -t 0 "$spec" \
        | gzip -n -9 >"$ARM64_ART_DIR/$out"
    echo "ok: $ARM64_ART_DIR/$out"
}

pack_initramfs "$arm64_init" initramfs.cpio.gz
pack_initramfs "$el0probe_init" initramfs-el0probe.cpio.gz
