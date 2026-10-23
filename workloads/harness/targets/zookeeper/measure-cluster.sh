#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
members=${1:?missing member count}
/opt/harmony/setup.sh "$members"
printf 'CLUSTER_START members=%s ' "$members"; cat /proc/uptime
pids=""
for node in $(seq 1 "$members"); do
    /opt/harmony/node.sh "$node" "$members" &
    pids="$pids $!"
done
status=0
ZOOKEEPER_MEMBERS=$members /opt/harmony/zookeeper-harness ready --ready-timeout 300s || status=$?
if [ "$status" -eq 0 ]; then
    printf 'CLUSTER_READY members=%s ' "$members"; cat /proc/uptime
else
    printf 'CLUSTER_FAILED members=%s ' "$members"; cat /proc/uptime
fi
for pid in $pids; do
    grep -E '^(Name|VmRSS|VmHWM|Threads):' /proc/$pid/status || true
done
cat /proc/meminfo
for pid in $pids; do kill -KILL "$pid" 2>/dev/null || true; done
for pid in $pids; do wait "$pid" 2>/dev/null || true; done
for node in $(seq 1 "$members"); do cat "/tmp/zookeeper/z$node.log"; done
exit "$status"
