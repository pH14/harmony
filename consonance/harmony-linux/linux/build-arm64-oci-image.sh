#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Compatibility entrypoint for the native arm64 platform runtime build. The
# implementation is shared with x86 so both architectures carry the same
# pinned runc, PID 1, supervisor, and generic userland contract.
set -euo pipefail

cd "$(dirname "$0")"
exec ./build-oci-runtime-initramfs.sh aarch64
