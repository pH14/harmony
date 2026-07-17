#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Build the STOCK linux-6.18.35 host kernel natively on the Altra box, as Debian
# packages (bindeb-pkg), from the same pinned tree the AA-3 patch targets
# (host/BUILD.md §0). Two reasons this exists (task 122, day one):
#
#  1. AA-1 host attestation: `arm-spike run` requires a build-id-bearing vmlinux ELF
#     matching the RUNNING kernel; Ubuntu publishes no ddeb vmlinux for the delivered
#     6.8.0-134, so the delivered kernel cannot be attested. A self-built host can.
#  2. AA-3 boots a PATCHED build of this same tree (arm64 KVM is built-in — every
#     patched cycle is a reboot). Building and booting the stock tree first de-risks
#     that cycle and makes stock-vs-patched a one-variable experiment.
#
# Config: Ubuntu's running config carried forward (olddefconfig with defaults), so
# the HPE RL300's storage/net drivers stay present; BTF and DWARF debug info off
# (build speed; the GNU build-id note is a linker artifact and survives), module
# signing keys cleared (no Ubuntu trusted-key material to inherit).
#
# Run detached: nohup bash build-stock-6.18.35.sh > ~/kernel/build.log 2>&1 &
# Success marker: ~/kernel/BUILD_OK (state-based wait; never pgrep).
set -euo pipefail

KERNEL_VERSION=6.18.35
KERNEL_URL=https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-6.18.35.tar.xz
KERNEL_SHA256=f78602932219125e211c5f5bfd84edcfd4ec5ce88fc944f8248413f665bef236

sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq \
  flex bison bc libssl-dev libelf-dev fakeroot dpkg-dev rsync kmod cpio zstd

mkdir -p ~/kernel && cd ~/kernel
if [[ ! -f linux-${KERNEL_VERSION}.tar.xz ]]; then
  curl -fsSL -o linux-${KERNEL_VERSION}.tar.xz "$KERNEL_URL"
fi
echo "$KERNEL_SHA256  linux-${KERNEL_VERSION}.tar.xz" | sha256sum -c -

rm -rf linux-${KERNEL_VERSION}
tar xf linux-${KERNEL_VERSION}.tar.xz
cd linux-${KERNEL_VERSION}

cp "/boot/config-$(uname -r)" .config
scripts/config --disable SYSTEM_TRUSTED_KEYS --disable SYSTEM_REVOCATION_KEYS
scripts/config --disable DEBUG_INFO_BTF
scripts/config --disable DEBUG_INFO_DWARF_TOOLCHAIN_DEFAULT --enable DEBUG_INFO_NONE
yes '' | make ARCH=arm64 olddefconfig

grep -E '^CONFIG_KVM=' .config
grep -E '^CONFIG_DEBUG_INFO(_NONE)?=' .config

make ARCH=arm64 -j"$(nproc)" bindeb-pkg

touch ~/kernel/BUILD_OK
echo "BUILD_OK: $(ls ../linux-image-*.deb)"
