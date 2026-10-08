#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
version=${1:?usage: build.sh affected|fixed}
case "$version" in
  affected) revision=3ce53bc469dcef8d8c2d90eb59a7d13184e782e5 ;;
  fixed) revision=ac1a538a559d94801b29a96f96fd8f9e0943f88f ;;
  *) exit 2 ;;
esac
export BUILDAH_FORMAT=docker
bash workloads/languages/build-image.sh python
"${HARMONY_CONTAINER_TOOL:-docker}" build --file workloads/languages/python/Dockerfile \
  --target compiled --tag harmony-language-python-builder:local .
"${HARMONY_CONTAINER_TOOL:-docker}" build \
  --file workloads/bugs/historical/sqlite-wal-reset/scenario/Dockerfile \
  --build-arg "SQLITE_COMMIT=$revision" --tag "harmony-sqlite-scenario:$version" .
