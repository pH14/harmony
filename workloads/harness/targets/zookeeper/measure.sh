#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/setup.sh 1
printf 'JVM_START '; cat /proc/uptime
/opt/java/bin/java "$@" -Dlogback.configurationFile=/opt/zookeeper/conf/logback.xml \
    -cp '/opt/zookeeper/lib/*' org.apache.zookeeper.server.quorum.QuorumPeerMain \
    /tmp/zookeeper/z1/config >> /tmp/zookeeper/z1.log 2>&1 &
server=$!
cleanup() {
    kill -KILL "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
}
trap cleanup EXIT
ZOOKEEPER_MEMBERS=1 /opt/harmony/zookeeper-harness ready --ready-timeout 300s
printf 'JVM_READY '; cat /proc/uptime
for task in /proc/$server/task/*; do
    cat "$task/stat" 2>/dev/null || true
done
cleanup
trap - EXIT
cat /tmp/zookeeper/z1.log
if [ -f /tmp/zookeeper/jvm.log ]; then cat /tmp/zookeeper/jvm.log; fi
