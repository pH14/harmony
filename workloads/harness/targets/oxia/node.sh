#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
address="10.77.0.${index}"

if [ "${index}" = 4 ]; then
  exec /opt/harmony/harness-net exec "${index}" -- /opt/oxia/oxia coordinator \
    --cconfig /opt/oxia/cluster.yaml \
    --metadata file \
    --file-clusters-status-path /tmp/oxia/c/cluster-status.json \
    --internal-addr "${address}:6649" \
    --admin-addr "${address}:6651" \
    --metrics-addr "${address}:8080" \
    >>/tmp/oxia/c.log 2>&1
fi

name="n${index}"
exec /opt/harmony/harness-net exec "${index}" -- /opt/oxia/oxia server \
  --public-addr "${address}:6648" \
  --internal-addr "${address}:6649" \
  --metrics-addr "${address}:8080" \
  --wal-dir "/tmp/oxia/${name}/wal" \
  --data-dir "/tmp/oxia/${name}/db" \
  --db-cache-size-mb 16 \
  >>"/tmp/oxia/${name}.log" 2>&1
