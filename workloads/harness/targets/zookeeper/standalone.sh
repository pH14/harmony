#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
mkdir -p /tmp/zookeeper
cat > /tmp/zookeeper/config <<'CONFIG'
tickTime=2000
dataDir=/tmp/zookeeper/data
clientPort=2181
admin.enableServer=false
4lw.commands.whitelist=ruok,srvr
CONFIG
printf 'JVM_START '; cat /proc/uptime
exec /opt/java/bin/java "$@" -Dlogback.configurationFile=/opt/zookeeper/conf/logback.xml \
    -cp '/opt/zookeeper/lib/*' org.apache.zookeeper.server.quorum.QuorumPeerMain /tmp/zookeeper/config
