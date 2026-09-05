#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the fault-library guest kernel -> bzImage-faultlab: the pinned guest
# kernel with ring-3 timestamp-counter reads left to the host.
#
# The historical database specimens fault on their first ring-3 RDTSC under the
# default kernel; see x86-faultlab-config-fragment for why, and for why this
# image is only deterministic on a host with the patched KVM loaded.
set -euo pipefail
cd "$(dirname "$0")"
FAULTLAB_TRAPS_OFF=1 exec ./build-kernel.sh "$@"
