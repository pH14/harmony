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

# The reproducibility and boot checks use the pinned ubuntu-24.04 toolchain's
# reviewed opcode baselines. Other build profiles select their own lists.
export HARMONY_RDTSC_ALLOWLIST=${HARMONY_RDTSC_ALLOWLIST:-$LINUX_DIR/rdtsc-allowlist-gha.txt}
export HARMONY_RDRAND_ALLOWLIST=${HARMONY_RDRAND_ALLOWLIST:-$LINUX_DIR/rdrand-allowlist-gha.txt}

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
./test-missing-host-clock.sh "$ART_DIR/bzImage" "$ART_DIR/initramfs.cpio.gz"
echo "PASS: guest Linux image checks"
