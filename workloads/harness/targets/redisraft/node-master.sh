#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"
address="10.77.0.${index}"
dir="/tmp/redisraft/${name}"

exec /opt/harmony/harness-net exec "${index}" -- /opt/redis/redis-server \
  --bind "${address}" \
  --port 6379 \
  --protected-mode no \
  --dir "${dir}" \
  --dbfilename snapshot.rdb \
  --save "" \
  --appendonly no \
  --loadmodule /opt/redis/redisraft.so \
  --raft.id "${index}" \
  --raft.addr "${address}:6379" \
  --raft.log-filename "${dir}/raftlog.db" \
  >>"/tmp/redisraft/${name}.log" 2>&1
