#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build M3's separate container-capable arm64 kernel without changing the
# sealed M1 or M2 kernel artifacts.
set -euo pipefail

cd "$(dirname "$0")"
ARM64_KERNEL_PROFILE=postgres ./build-arm64-kernel.sh
