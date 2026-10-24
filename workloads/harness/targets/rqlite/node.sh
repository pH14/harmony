#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

name=${1:?missing node name}
http_port=${2:?missing HTTP port}
raft_port=${3:?missing Raft port}
index=${name#n}
address="10.77.0.${index}"
joins='10.77.0.1:4002,10.77.0.2:4004,10.77.0.3:4006'

exec /opt/harmony/harness-net exec "${index}" -- /opt/rqlite/rqlited \
  -node-id "${name}" \
  -http-addr "${address}:${http_port}" \
  -raft-addr "${address}:${raft_port}" \
  -bootstrap-expect 3 \
  -join "${joins}" \
  "/tmp/rqlite/${name}" \
  >>"/tmp/rqlite/${name}.log" 2>&1
