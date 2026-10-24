#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"
address="10.77.0.${index}"
dir="/tmp/summitdb/${name}"

# A server exits when its join request fails, so a new member waits until
# node 1 reports a leader. A member with a raft log already knows its peers
# and ignores -join.
join=""
if [ "${index}" != 1 ]; then
  join="-join 10.77.0.1:7481"
  if [ ! -e "${dir}/raft.db" ]; then
    /opt/harmony/harness-net exec "${index}" -- /opt/harmony/summitdb-harness wait-leader 10.77.0.1:7481
  fi
fi

exec /opt/harmony/harness-net exec "${index}" -- /opt/summitdb/summitdb-server \
  -h "${address}" \
  -p 7481 \
  -dir "${dir}" \
  ${join} \
  >>"/tmp/summitdb/${name}.log" 2>&1
