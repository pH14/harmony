#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

name=${1:?missing node name}
index=${2:?missing node index}
address="10.77.0.${index}"
routes='nats://10.77.0.1:6222,nats://10.77.0.2:6222,nats://10.77.0.3:6222'

# The image carries a server config only when it was built with NATS_SYNC=always.
set --
if [ -f /opt/harmony/nats.conf ]; then
  set -- --config /opt/harmony/nats.conf
fi

exec /opt/harmony/harness-net exec "${index}" -- /opt/nats/nats-server "$@" \
  --name "${name}" \
  --addr "${address}" \
  --port 4222 \
  --jetstream \
  --store_dir "/tmp/nats/${name}" \
  --cluster_name harmony \
  --cluster "nats://${address}:6222" \
  --routes "${routes}" \
  >>"/tmp/nats/${name}.log" 2>&1
