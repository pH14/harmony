#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"

exec /opt/harmony/harness-net exec "${index}" -- /opt/dqlite/dqlite-node \
  "${index}" "10.77.0.${index}:9001" "/tmp/dqlite/${name}" \
  >>"/tmp/dqlite/${name}.log" 2>&1
