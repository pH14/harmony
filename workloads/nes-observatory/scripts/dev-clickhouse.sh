#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

name=${HARMONY_OBSERVATORY_CONTAINER:-harmony-observatory-clickhouse}
data=${HARMONY_OBSERVATORY_DATA:-$PWD/.observatory-dev/clickhouse}
user=${HARMONY_CLICKHOUSE_USER:-observatory}
password=${HARMONY_CLICKHOUSE_PASSWORD:?set HARMONY_CLICKHOUSE_PASSWORD}
engine=${HARMONY_CONTAINER_ENGINE:-podman}
mkdir -p "$data"
exec "$engine" run --name "$name" --replace --detach \
  --cpus=4 --memory=8g --memory-swap=8g \
  -p 127.0.0.1:8123:8123 \
  -v "$data:/var/lib/clickhouse:Z" \
  -e CLICKHOUSE_USER="$user" \
  -e CLICKHOUSE_PASSWORD="$password" \
  docker.io/library/clickhouse:26.3.33.24
