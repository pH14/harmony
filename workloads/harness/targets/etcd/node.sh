#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing member index}
members=${2:?missing member count}
name="m${index}"
address="10.77.0.${index}"
cluster=""
i=1
while [ "$i" -le "$members" ]; do
  cluster="${cluster:+${cluster},}m${i}=http://10.77.0.${i}:2380"
  i=$((i + 1))
done
# harness-net needs two nodes; a single member runs on loopback.
run="/opt/harmony/harness-net exec ${index} --"
if [ "$members" -eq 1 ]; then
  address=127.0.0.1
  cluster="m1=http://127.0.0.1:2380"
  run=""
fi

exec ${run} /opt/etcd/etcd \
  --name "${name}" \
  --data-dir "/tmp/etcd/${name}" \
  --listen-client-urls "http://${address}:2379" \
  --advertise-client-urls "http://${address}:2379" \
  --listen-peer-urls "http://${address}:2380" \
  --initial-advertise-peer-urls "http://${address}:2380" \
  --initial-cluster "${cluster}" \
  --initial-cluster-state new \
  --initial-cluster-token harmony-harness \
  >>"/tmp/etcd/${name}.log" 2>&1
