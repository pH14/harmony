#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the arm64 OCI initramfs from macOS via an arm64 Linux container.
set -euo pipefail
cd "$(dirname "$0")/../../.."
exec docker run --rm --platform linux/arm64 -v "$PWD":/work -w /work debian:stable \
    bash -c 'apt-get update -qq >/dev/null && \
             apt-get install -y -qq build-essential flex bison bc cpio python3 gzip patch bzip2 >/dev/null && \
             make -C consonance/harmony-linux arm64-oci-image'
