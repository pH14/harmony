#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
index=${1:?missing index}
members=${2:-3}
shift 2
run="/opt/harmony/harness-net exec $index --"
[ "$members" -ne 1 ] || run=""
exec $run /opt/java/bin/java -XX:TieredStopAtLevel=1 -XX:+UseSerialGC \
    -XX:ActiveProcessorCount=1 -Xms32m -Xmx128m "$@" \
    -Dlogback.configurationFile=/opt/zookeeper/conf/logback.xml \
    -cp '/opt/zookeeper/lib/*' org.apache.zookeeper.server.quorum.QuorumPeerMain \
    "/tmp/zookeeper/z$index/config" >> "/tmp/zookeeper/z$index.log" 2>&1
