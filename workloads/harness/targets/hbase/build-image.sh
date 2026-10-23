#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/../../../.."
recipe=$PWD/workloads/harness/targets/hbase
cache=${HARMONY_HBASE_BUILD_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/harmony/hbase-build}
mkdir -p "$cache/downloads"
limits=(--memory "${HARMONY_HBASE_BUILD_MEMORY:-24g}")
if [ -n "${HARMONY_HBASE_BUILD_CPUS:-}" ]; then limits+=(--cpuset-cpus "$HARMONY_HBASE_BUILD_CPUS"); fi
fetch() {
 local url=$1 name=$2
 if [ ! -f "$cache/downloads/$name" ]; then
  curl --fail --location --retry 3 "$url" -o "$cache/downloads/$name.pending"
  mv "$cache/downloads/$name.pending" "$cache/downloads/$name"
 fi
}
fetch https://archive.apache.org/dist/hbase/3.0.0/hbase-3.0.0-bin.tar.gz hbase-3.0.0-bin.tar.gz
fetch https://archive.apache.org/dist/hadoop/common/hadoop-3.4.3/hadoop-3.4.3.tar.gz hadoop-3.4.3.tar.gz
podman build "${limits[@]}" --format docker -f "$recipe/source.Dockerfile" -t harmony-hbase-build:local "$cache"
podman build "${limits[@]}" --format docker -f "$recipe/hadoop.Dockerfile" -t harmony-hadoop-build:local "$cache"
export HARMONY_CONTAINER_TOOL=podman
bash workloads/languages/build-image.sh java "${limits[@]}" --build-arg JAVA_MODULES=java.se,jdk.unsupported,jdk.security.auth,jdk.management,jdk.naming.dns,jdk.crypto.ec,jdk.zipfs
podman tag harmony-language-java:local harmony-hbase-java:local
podman build "${limits[@]}" --format docker -f workloads/harness/targets/zookeeper/Dockerfile --build-arg JAVA_IMAGE=harmony-hbase-java:local -t harmony-zookeeper:hbase .
podman build "${limits[@]}" --format docker -f "$recipe/Dockerfile" -t harmony-hbase:local .
