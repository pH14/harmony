#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
members=${1:-3}
[ "$members" -eq 1 ] || /opt/harmony/harness-net setup "$members"
mkdir -p /tmp/zookeeper /tmp/harness/journal
for index in $(seq 1 "$members"); do
    dir=/tmp/zookeeper/z$index
    mkdir -p "$dir/data"
    printf '%s\n' "$index" > "$dir/data/myid"
    cat > "$dir/config" <<CONFIG
tickTime=2000
initLimit=10
syncLimit=5
dataDir=$dir/data
clientPort=2181
admin.enableServer=false
4lw.commands.whitelist=ruok,srvr
CONFIG
    if [ "$members" -gt 1 ]; then
        for peer in $(seq 1 "$members"); do
            printf 'server.%s=10.77.0.%s:2888:3888\n' "$peer" "$peer" >> "$dir/config"
        done
    fi
done
