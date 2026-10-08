#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
export BUILDAH_FORMAT=docker
bash workloads/languages/build-image.sh python
"${HARMONY_CONTAINER_TOOL:-docker}" build --file workloads/languages/python/Dockerfile \
  --target compiled --tag harmony-language-python-builder:local .
"${HARMONY_CONTAINER_TOOL:-docker}" build \
  --file workloads/bugs/historical/sqlite-wal-reset/scenario/Dockerfile \
  --tag harmony-sqlite-scenario:affected .
