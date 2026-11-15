#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Each node runs one Zero and one Alpha. When either exits, the other is
# killed, so the supervisor sees one node death and restarts both.
set -eu
index=${1:?missing node index}
name="n${index}"
address="10.77.0.${index}"
dir="/tmp/dgraph/${name}"
log="/tmp/dgraph/${name}.log"
telemetry="reports=false; sentry=false;"
peer=()
if [ "${index}" != 1 ]; then
  peer=(--peer 10.77.0.1:5080)
fi
mkdir -p "${dir}/t"
cd "${dir}"
/opt/harmony/harness-net exec "${index}" -- /opt/dgraph/dgraph zero \
  --my "${address}:5080" \
  --raft "idx=${index}" \
  --replicas 3 \
  "${peer[@]}" \
  --wal "${dir}/zw" \
  --survive filesystem \
  --telemetry "${telemetry}" \
  >>"${log}" 2>&1 &
zero=$!
/opt/harmony/harness-net exec "${index}" -- /opt/dgraph/dgraph alpha \
  --my "${address}:7080" \
  --zero 10.77.0.1:5080,10.77.0.2:5080,10.77.0.3:5080 \
  --postings "${dir}/p" \
  --wal "${dir}/w" \
  --tmp "${dir}/t" \
  --survive filesystem \
  --cache "size-mb=64" \
  --security "whitelist=10.77.0.0/24" \
  --telemetry "${telemetry}" \
  >>"${log}" 2>&1 &
alpha=$!
status=0
wait -n "${zero}" "${alpha}" || status=$?
kill -KILL "${zero}" "${alpha}" 2>/dev/null || true
wait || true
echo "node ${name}: a process exited with status ${status}" >>"${log}"
exit "${status:-1}"
