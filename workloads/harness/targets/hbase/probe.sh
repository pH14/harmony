#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/hbase-setup.sh
printf 'HBASE_START '; cat /proc/uptime
pids=""
cleanup() {
 for pid in $pids; do kill -KILL "$pid" 2>/dev/null || true; done
 for pid in $pids; do wait "$pid" 2>/dev/null || true; done
 for file in /tmp/hbase/n*/*.log /tmp/zookeeper/z?.log; do
  [ -f "$file" ] || continue
  printf '\nLOG %s\n' "$file"
  tail -80 "$file"
 done
}
trap cleanup EXIT
while read -r kind name command; do
 [ "$kind" = node ] || continue
 sh -c "exec $command" &
 pids="$pids $!"
done < /etc/harmony/bundle
/opt/harmony/hbase-harness ready --ready-timeout 540s
printf 'HBASE_READY_TIME '; cat /proc/uptime
cat /proc/meminfo
for pid in $pids; do cat /proc/$pid/status; done
/opt/harmony/hbase-harness drive --processes 2 --ops 10 --timeout 20s
/opt/harmony/hbase-harness check --check-timeout 90s
