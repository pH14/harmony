# SPDX-License-Identifier: AGPL-3.0-or-later
# shellcheck shell=bash
# shellcheck disable=SC2034  # variables are consumed by the sourcing scripts
# Shared setup for the guest Linux image scripts. Source from guest/linux/
# with CWD = guest/linux/. Every entry point selects its native build host
# explicitly: the established x86 image uses Linux/x86_64; the AA-5(c) image
# uses Linux/aarch64 on the pinned Altra box.

# shellcheck source=../scripts/lib.sh disable=SC1091
. ../scripts/lib.sh
# shellcheck source=versions.lock disable=SC1091
. ./versions.lock

# Fail fast (never skip silently) when not on Linux/x86_64.
require_linux_amd64() {
    if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
        echo "FAIL: the guest Linux image builds and tests only on Linux/x86_64." >&2
        echo "      On macOS run it in a linux/amd64 container — see docs/BUILDING.md." >&2
        exit 1
    fi
}

# Fail fast (never cross-build silently) when the AA-5(c) artifacts are not
# being built on the pinned Linux/aarch64 execution host. Native builds keep
# the compiler and absolute build paths inside the recorded box contract.
require_linux_aarch64() {
    if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "aarch64" ]; then
        echo "FAIL: the AA-5(c) guest image builds only on Linux/aarch64." >&2
        echo "      Run it natively on the pinned Altra box — see docs/ARM-ALTRA.md." >&2
        exit 1
    fi
}

# require_tools <tool...>: fail fast with the install hint if any is missing.
require_tools() {
    missing=""
    for t in "$@"; do
        command -v "$t" >/dev/null 2>&1 || missing="$missing $t"
    done
    if [ -n "$missing" ]; then
        echo "FAIL: missing host tools:$missing" >&2
        echo "      see guest/README.md for the package list (Debian: apt-get install ...)" >&2
        exit 1
    fi
}

GUEST_DIR=$(cd .. && pwd)
LINUX_DIR=$GUEST_DIR/linux
DL_DIR=$GUEST_DIR/dl
# Final artifacts (bzImage, initramfs.cpio.gz) live on the repo side so they
# survive the container session.
ART_DIR=$GUEST_DIR/build
# AA-5(c) artifacts are isolated from the x86 image. In particular, an ARM
# build must never overwrite the canonical bzImage/initramfs pair consumed by
# the established x86 gates.
ARM64_ART_DIR=$ART_DIR/arm64

# Build trees live at a fixed absolute path on a native filesystem:
# - fixed, so absolute paths are identical between the two reproducibility
#   builds (the kernel embeds none, but O= must not differ);
# - native (not the bind-mounted repo), because the repo may sit on a
#   case-insensitive macOS filesystem, and a kernel tree cannot be extracted
#   onto one (case-colliding header names).
# "hypervizor" here is a hashed MANIFEST.sha256 input (build path embedded in the
# reproducible-build recipe) — deliberately NOT renamed to "harmony" post-task-90.
# See tasks/90-rename-harmony.md's close-out note; ruled document-as-deliberately-stale
# by task 62, not a rebaseline. Do not touch this string.
BUILD_ROOT=${GUEST_BUILD_ROOT:-/tmp/hypervizor-guest-build}

KSRC=$BUILD_ROOT/linux-$KERNEL_VERSION
KOBJ=$BUILD_ROOT/kernel-build
BBSRC=$BUILD_ROOT/busybox-$BUSYBOX_VERSION
BBOBJ=$BUILD_ROOT/busybox-build
ARM64_KOBJ=$BUILD_ROOT/kernel-build-arm64
ARM64_BBOBJ=$BUILD_ROOT/busybox-build-arm64

# Reproducibility levers (task spec): fixed timestamp/user/host/version, fixed
# SOURCE_DATE_EPOCH, no kconfig header timestamps. LOCALVERSION is fixed in
# the config fragment (empty, LOCALVERSION_AUTO=n).
export KBUILD_BUILD_TIMESTAMP='Thu Jan  1 00:00:00 UTC 1970'
# "hypervizor" (both vars below) is a hashed MANIFEST.sha256 input — deliberately NOT
# renamed post-task-90; see tasks/90-rename-harmony.md's close-out note (task 62 ruling:
# document-as-deliberately-stale). Do not touch these strings.
export KBUILD_BUILD_USER=hypervizor
export KBUILD_BUILD_HOST=hypervizor
export KBUILD_BUILD_VERSION=1
export SOURCE_DATE_EPOCH=0
export KCONFIG_NOTIMESTAMP=1

# verify_and_extract <tarball> <sha256> <target-dir>: hash-check the download
# (always — "verify the hash before building") and extract it under
# BUILD_ROOT if the source tree is not already there.
verify_and_extract() {
    tarball=$1
    sha=$2
    dir=$3
    if [ ! -f "$tarball" ]; then
        echo "FAIL: $tarball missing — run 'make -C guest fetch' first (needs network once)" >&2
        exit 1
    fi
    got=$(sha256_of "$tarball")
    if [ "$got" != "$sha" ]; then
        echo "FAIL: $tarball sha256 mismatch (want $sha, got $got)" >&2
        exit 1
    fi
    if [ ! -d "$dir" ]; then
        mkdir -p "$BUILD_ROOT"
        tar -xf "$tarball" -C "$BUILD_ROOT"
    fi
}

extract_kernel() {
    verify_and_extract "$DL_DIR/$(basename "$KERNEL_URL")" "$KERNEL_SHA256" "$KSRC"
}

extract_busybox() {
    verify_and_extract "$DL_DIR/$(basename "$BUSYBOX_URL")" "$BUSYBOX_SHA256" "$BBSRC"
}
