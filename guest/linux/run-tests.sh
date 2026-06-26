#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Part B gates.
#   1. Reproducibility: clean-artifacts + image, twice; bzImage and
#      initramfs.cpio.gz sha256s must be identical across the two builds;
#      emits guest/linux/MANIFEST.sha256.
#   2. Boot: the (second-build) image boots under QEMU, prints GUEST_READY
#      within 120 s, and QEMU exits via the guest's poweroff.
# Repro runs first so the boot test exercises exactly the manifested bytes.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools qemu-system-x86_64

build_once() {
    ./clean-artifacts.sh
    ./build-kernel.sh
    ./build-initramfs.sh
}

echo "== repro test: build #1"
build_once
k1=$(sha256_of "$ART_DIR/bzImage")
i1=$(sha256_of "$ART_DIR/initramfs.cpio.gz")

echo "== repro test: build #2"
build_once
k2=$(sha256_of "$ART_DIR/bzImage")
i2=$(sha256_of "$ART_DIR/initramfs.cpio.gz")

if [ "$k1" != "$k2" ] || [ "$i1" != "$i2" ]; then
    echo "FAIL: builds are not reproducible" >&2
    echo "      bzImage:           $k1" >&2
    echo "                  vs     $k2" >&2
    echo "      initramfs.cpio.gz: $i1" >&2
    echo "                  vs     $i2" >&2
    exit 1
fi
printf '%s  bzImage\n%s  initramfs.cpio.gz\n' "$k1" "$i1" >MANIFEST.sha256
echo "ok: two builds bit-identical; MANIFEST.sha256 written"

echo "== boot test"
out=$(mktemp)
status=0
# -machine hpet=off and random.trust_cpu=off apply the runtime mitigations
# the config-fragment documents (HPET_TIMER cannot be configured out on
# x86-64; RDRAND crediting is a boot parameter since kernel 6.2), so the
# gate boots the time/entropy surface the fragment claims. Expected with no
# HPET and no PM timer: under (nested) TCG the kernel may fail PIT-based TSC
# calibration and boot on jiffies — proof that no other hardware clocksource
# is reachable; the hypervisor will hand the guest its TSC frequency via
# controlled CPUID/MSR surfaces instead of calibration.
run_with_timeout 120 qemu-system-x86_64 \
    -m 512 -nographic -no-reboot \
    -machine hpet=off \
    -kernel "$ART_DIR/bzImage" \
    -initrd "$ART_DIR/initramfs.cpio.gz" \
    -append "console=ttyS0 panic=-1 random.trust_cpu=off" \
    </dev/null >"$out" 2>&1 || status=$?
if [ "$status" -eq 124 ]; then
    echo "FAIL: boot test timed out after 120 s (no poweroff)" >&2
    tr -d '\r' <"$out" | tail -30 >&2
    rm -f "$out"
    exit 1
fi
if [ "$status" -ne 0 ]; then
    echo "FAIL: QEMU exited with status $status (want 0 via guest poweroff)" >&2
    tr -d '\r' <"$out" | tail -30 >&2
    rm -f "$out"
    exit 1
fi
if ! grep -q 'GUEST_READY' "$out"; then
    echo "FAIL: GUEST_READY not seen on the console (QEMU exit status $status)" >&2
    tr -d '\r' <"$out" | tail -30 >&2
    rm -f "$out"
    exit 1
fi
rm -f "$out"
echo "ok: GUEST_READY seen and QEMU exited (status $status)"
echo "PASS: guest Linux image gates"
