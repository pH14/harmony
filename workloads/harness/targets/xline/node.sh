#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
address="10.77.0.${index}"
data="/tmp/xline/n${index}/data"
members="n1=10.77.0.1:2380,n2=10.77.0.2:2380,n3=10.77.0.3:2380"
# Upstream starts the first member with --is-leader. A restarted member
# already holds its term and vote, so only an empty data directory gets it.
leader=""
if [ "${index}" -eq 1 ] && [ ! -d "${data}" ]; then
  leader="--is-leader"
fi
exec /opt/harmony/harness-net exec "${index}" -- /opt/xline/xline \
  --name "n${index}" \
  --members "${members}" \
  --storage-engine rocksdb \
  --data-dir "${data}" \
  --client-listen-urls "http://${address}:2379" \
  --client-advertise-urls "http://${address}:2379" \
  --peer-listen-urls "http://${address}:2380" \
  --peer-advertise-urls "http://${address}:2380" \
  --initial-cluster-state new \
  ${leader} \
  >>"/tmp/xline/n${index}.log" 2>&1
