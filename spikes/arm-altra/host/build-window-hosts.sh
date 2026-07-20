#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# 2026-07-20 box-window host-kernel reconstruction. The re-provisioned box lost
# ~/kernel and the installed stock 6.18.35 (so /boot/config-6.18.35 is gone);
# this rebuilds the two PATCHED host kernels the live gates need, natively:
#
#   -aa3preempt : stock 6.18.35 + 0001 (KVM_EXIT_PREEMPT force-exit) — the AA-3
#                 mechanism kernel; the AA-5(c) live gates boot this.
#   -aa4guard   : stock 6.18.35 + 0001 + 0002 (stage-2 execute guard, cap 246) —
#                 hm-rfz's planted-proof kernel, booted after the AA-5 gates.
#
# Config discipline mirrors host/build-stock-6.18.35.sh exactly (running config
# carried forward, trusted keys cleared, BTF/DWARF off) with one recorded
# deviation: the 6.18.35 base config is regenerated from the RUNNING 6.8.0-134
# config via olddefconfig (the wipe deleted /boot/config-6.18.35; the original
# stock build derived its config from the running kernel the same way).
#
# Run detached: nohup setsid bash build-window-hosts.sh </dev/null > ~/kernel/build-window.log 2>&1 &
# Success markers: ~/kernel/BUILD_AA3PREEMPT_OK then ~/kernel/BUILD_AA4GUARD_OK
# (state-based wait; never pgrep).
set -euo pipefail

KERNEL_VERSION=6.18.35
KERNEL_SHA256=f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236
PATCH_DIR=$HOME/harmony/spikes/arm-altra/host/patches
P1=$PATCH_DIR/0001-KVM-arm64-add-KVM_EXIT_PREEMPT-in-kernel-force-exit-.patch
P2=$PATCH_DIR/0002-KVM-arm64-add-userspace-stage-2-execute-guard.patch
P1_SHA256=069c226ad2dfe87d31ea5147875088c0244c6c87d8d66bf58d5757bfeeca9555
P2_SHA256=b51a75f6daad7ad8c8f50e2384bb170660acf429dabc2e72058b2771705736ad

sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  flex bison bc libssl-dev libelf-dev fakeroot dpkg-dev rsync kmod cpio zstd

mkdir -p ~/kernel && cd ~/kernel
if [[ ! -f linux-${KERNEL_VERSION}.tar.xz ]]; then
  # The pinned tarball the guest-kernel build already fetched and verified.
  cp ~/harmony/guest/dl/linux-${KERNEL_VERSION}.tar.xz .
fi
echo "$KERNEL_SHA256  linux-${KERNEL_VERSION}.tar.xz" | sha256sum -c -
echo "$P1_SHA256  $P1" | sha256sum -c -
echo "$P2_SHA256  $P2" | sha256sum -c -

# build_one <tree-dir> <localversion> <patch>...: fresh pristine extract, apply
# the patch stack, assert the mechanism symbols landed, config per the recorded
# discipline, bindeb-pkg.
build_one() {
  tree=$1
  localversion=$2
  shift 2
  echo "==> [$localversion] fresh extract into $tree"
  rm -rf "$tree"
  mkdir -p "$tree"
  tar xf ~/kernel/linux-${KERNEL_VERSION}.tar.xz -C "$tree" --strip-components=1
  cd "$tree"
  for p in "$@"; do
    echo "==> [$localversion] apply $(basename "$p")"
    patch -p1 --no-backup-if-mismatch <"$p"
  done
  # Fail closed if the hunks silently no-op'd.
  grep -rq "KVM_EXIT_PREEMPT" include/uapi/linux/kvm.h || { echo "FAIL: KVM_EXIT_PREEMPT not in kvm.h" >&2; exit 1; }
  grep -rq "preempt_armed" arch/arm64/include/asm/kvm_host.h || { echo "FAIL: preempt_armed not in kvm_host.h" >&2; exit 1; }
  grep -rq "KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS" include/uapi/linux/kvm.h || { echo "FAIL: determinism cap not in kvm.h" >&2; exit 1; }
  if [ "$localversion" = "-aa4guard" ]; then
    grep -rq "KVM_EXIT_ARM_STAGE2_EXEC_GUARD" include/uapi/linux/kvm.h || { echo "FAIL: exec-guard exit not in kvm.h" >&2; exit 1; }
    grep -rq "KVM_ARCH_FLAG_STAGE2_EXEC_GUARD" arch/arm64/include/asm/kvm_host.h || { echo "FAIL: exec-guard flag not in kvm_host.h" >&2; exit 1; }
  fi
  echo "==> [$localversion] patches applied; mechanism symbols present"

  cp "/boot/config-$(uname -r)" .config
  scripts/config --disable SYSTEM_TRUSTED_KEYS --disable SYSTEM_REVOCATION_KEYS
  scripts/config --disable DEBUG_INFO_BTF
  scripts/config --disable DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT --enable DEBUG_INFO_NONE
  scripts/config --set-str LOCALVERSION "$localversion"
  scripts/config --disable LOCALVERSION_AUTO
  yes '' | make ARCH=arm64 olddefconfig
  grep -E '^CONFIG_KVM=' .config
  grep -E '^CONFIG_LOCALVERSION=' .config

  echo "==> [$localversion] build bindeb-pkg (-j$(nproc))"
  make ARCH=arm64 -j"$(nproc)" bindeb-pkg
  cd ~/kernel
}

build_one ~/kernel/linux-${KERNEL_VERSION}-aa3preempt -aa3preempt "$P1"
touch ~/kernel/BUILD_AA3PREEMPT_OK
echo "BUILD_AA3PREEMPT_OK: $(ls ~/kernel/linux-image-${KERNEL_VERSION}-aa3preempt_*.deb)"

build_one ~/kernel/linux-${KERNEL_VERSION}-aa4guard -aa4guard "$P1" "$P2"
touch ~/kernel/BUILD_AA4GUARD_OK
echo "BUILD_AA4GUARD_OK: $(ls ~/kernel/linux-image-${KERNEL_VERSION}-aa4guard_*.deb)"
