#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Build the PATCHED linux-6.18.35 host kernel natively on the Altra box (AA-3): stock
# linux-6.18.35 + host/patches/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch,
# as Debian packages, with a distinct LOCALVERSION so it installs alongside the stock 6.18.35
# with its own build-id and its own /boot entry. Stock-vs-patched is then a one-variable
# experiment (the patch). arm64 KVM is built-in, so the patch is exercised only after a reboot
# into this kernel.
#
# Mirrors host/build-stock-6.18.35.sh (same config discipline: running config carried forward,
# trusted keys cleared, BTF/DWARF off). Applies the patch with `patch -p1` (the box tree is a
# tarball, not a git repo, so `git am` — as host/verify.sh uses in its container — is not
# available; the diff hunks apply the same).
#
# Run detached: nohup setsid bash build-patched-6.18.35.sh </dev/null > ~/kernel/build-patched.log 2>&1 &
# Success marker: ~/kernel/BUILD_PATCHED_OK (state-based wait; never pgrep).
set -euo pipefail

KERNEL_VERSION=6.18.35
LOCALVERSION="-aa3preempt"
SRC=~/kernel/linux-${KERNEL_VERSION}                 # the stock tree (has the tarball + .config)
PATCHED=~/kernel/linux-${KERNEL_VERSION}-patched
PATCH=~/harmony/spikes/arm-altra/host/patches/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch

cd ~/kernel
[[ -f linux-${KERNEL_VERSION}.tar.xz ]] || { echo "FAIL: stock tarball missing; run build-stock first" >&2; exit 1; }
[[ -f "$PATCH" ]] || { echo "FAIL: patch not found at $PATCH" >&2; exit 1; }

echo "==> fresh-extract patched tree into $PATCHED (WITHOUT touching the stock tree)"
# BUG FIX: the first version did `tar xf` (which extracts to ./linux-6.18.35 — the SAME dir
# the stock tree lives in, clobbering it) then `mv linux-6.18.35 -patched` (moving the stock
# tree, including its vmlinux, which the patched build then overwrote). Extract straight into
# $PATCHED with --strip-components so the stock ~/kernel/linux-6.18.35 tree is never touched.
rm -rf "$PATCHED"
mkdir -p "$PATCHED"
tar xf linux-${KERNEL_VERSION}.tar.xz -C "$PATCHED" --strip-components=1
cd "$PATCHED"

echo "==> apply the patch (patch -p1)"
patch -p1 --no-backup-if-mismatch < "$PATCH"
# Assert the mechanism landed (fail closed if the hunks silently no-op'd).
grep -rq "KVM_EXIT_PREEMPT" include/uapi/linux/kvm.h || { echo "FAIL: KVM_EXIT_PREEMPT not in kvm.h" >&2; exit 1; }
grep -rq "preempt_armed" arch/arm64/include/asm/kvm_host.h || { echo "FAIL: preempt_armed not in kvm_host.h" >&2; exit 1; }
grep -rq "KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS" include/uapi/linux/kvm.h || { echo "FAIL: cap not in kvm.h" >&2; exit 1; }
echo "    patch applied; mechanism symbols present"

echo "==> config (running config carried forward + LOCALVERSION $LOCALVERSION)"
cp "/boot/config-6.18.35" .config
scripts/config --disable SYSTEM_TRUSTED_KEYS --disable SYSTEM_REVOCATION_KEYS
scripts/config --disable DEBUG_INFO_BTF
scripts/config --disable DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT --enable DEBUG_INFO_NONE
scripts/config --set-str LOCALVERSION "$LOCALVERSION"
scripts/config --disable LOCALVERSION_AUTO
make ARCH=arm64 olddefconfig </dev/null
grep -E '^CONFIG_KVM=' .config
grep -E '^CONFIG_LOCALVERSION=' .config

echo "==> build bindeb-pkg (-j$(nproc))"
make ARCH=arm64 -j"$(nproc)" bindeb-pkg

touch ~/kernel/BUILD_PATCHED_OK
echo "BUILD_PATCHED_OK: $(ls ~/kernel/linux-image-${KERNEL_VERSION}${LOCALVERSION}_*.deb 2>/dev/null || ls ~/kernel/linux-image-*aa3*.deb)"
