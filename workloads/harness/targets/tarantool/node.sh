#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"

exec /opt/harmony/harness-net exec "${index}" -- /opt/tarantool/tarantool \
  --name "${name}" \
  --config /opt/tarantool/config.yaml \
  >>"/tmp/tarantool/${name}.log" 2>&1
