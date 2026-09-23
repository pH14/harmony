#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Part B checks.
#   1. Reproducibility: clean-artifacts + image, twice; bzImage and
#      initramfs.cpio.gz sha256s must be identical across the two builds;
#      emits consonance/harmony-linux/linux/MANIFEST.sha256.
#   2. Negative boot: QEMU lacks Harmony's required clock doorbell, so the
#      shipped kernel must stop before /init with a registration panic.
# Repro runs first so the boot check exercises exactly the manifested bytes.
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

echo "== missing-host-interface boot test"
out=$(mktemp)
status=0
run_with_timeout 120 qemu-system-x86_64 \
    -m 512 -nographic -no-reboot \
    -machine hpet=off \
    -kernel "$ART_DIR/bzImage" \
    -initrd "$ART_DIR/initramfs.cpio.gz" \
    -append "console=ttyS0 panic=1 random.trust_cpu=off" \
    </dev/null >"$out" 2>&1 || status=$?
if [ "$status" -eq 124 ] || ! grep -q 'Kernel panic - not syncing: Harmony pvclock registration failed' "$out" || grep -q 'GUEST_READY' "$out"; then
    echo "FAIL: guest did not reject the missing Harmony host clock" >&2
    tr -d '\r' <"$out" | tail -30 >&2
    rm -f "$out"
    exit 1
fi
rm -f "$out"
echo "ok: missing Harmony clock stopped boot before /init (QEMU status $status)"
echo "PASS: guest Linux image checks"
