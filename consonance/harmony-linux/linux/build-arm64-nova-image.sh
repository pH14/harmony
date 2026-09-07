#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Compatibility entrypoint; NES image recipes are owned by the package.
set -euo pipefail
repo_root=$(cd "$(dirname "$0")/../../.." && pwd)
exec "$repo_root/workloads/nes-guest/legacy/build-arm64-nova-image.sh" "$@"
