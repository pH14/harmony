#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the native ARM64 Nova kernel profile and its restore-oracle image.
set -euo pipefail

package_dir=$(cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(cd "$package_dir/../.." && pwd)
linux_dir=$repo_root/consonance/harmony-linux/linux

cd "$linux_dir"
# shellcheck source=../../consonance/harmony-linux/linux/lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_aarch64

kernel_output=Image-nova
kernel_fragment=$package_dir/arm64-nova-config-fragment
kernel_required_y='BINFMT_SCRIPT PROC_FS PROC_SYSCTL PROC_PAGE_MONITOR SYSCTL FUTEX MMU SHMEM TMPFS DEVMEM HUGETLBFS COMPACTION MIGRATION'
kernel_required_off=STRICT_DEVMEM

ARM64_KERNEL_PROFILE=external \
ARM64_KERNEL_PROFILE_NAME=nova \
ARM64_KERNEL_OUTPUT="$kernel_output" \
ARM64_KERNEL_CONFIG_FRAGMENT="$kernel_fragment" \
ARM64_KERNEL_REQUIRED_Y="$kernel_required_y" \
ARM64_KERNEL_REQUIRED_OFF="$kernel_required_off" \
    "$linux_dir/build-arm64-kernel.sh"

"$package_dir/legacy/build-arm64-nova-image.sh"
