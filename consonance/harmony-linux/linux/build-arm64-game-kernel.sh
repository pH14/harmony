#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the separate M2 userspace-capable arm64 kernel without changing M1's
# canonical Image or object tree.
set -euo pipefail

cd "$(dirname "$0")"
ARM64_KERNEL_PROFILE=game ./build-arm64-kernel.sh
