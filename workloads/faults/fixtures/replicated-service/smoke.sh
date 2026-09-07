#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Linux smoke for the real fixture. It requires ip/tc and CAP_NET_ADMIN.
set -eu

root=$(CDPATH="" cd -- "$(dirname -- "$0")" && pwd)
bin=${BIN_DIR:-$root/target/debug}
primary_port=${PRIMARY_PORT:-17000}
replica_port=${REPLICA_PORT:-17001}
socket=${REPLICA_SOCKET:-${TMPDIR:-/tmp}/fault-replica-$$.sock}
primary_pid=''
replica_pid=''

cleanup() {
    [ -z "$primary_pid" ] || kill "$primary_pid" 2>/dev/null || true
    [ -z "$replica_pid" ] || kill "$replica_pid" 2>/dev/null || true
    tc qdisc del dev veth-0-1 root 2>/dev/null || true
    rm -f "$socket"
    for ns in fault-primary fault-replica; do
        if ip netns list | grep -Fqx "$ns"; then
            ip netns del "$ns" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT INT TERM

"$root/network-init.sh"
"$root/netns-exec" fault-primary "$bin/fault-replica" --role primary \
    --listen "10.0.0.1:$primary_port" --peer "10.0.1.1:$replica_port" \
    >/tmp/fault-primary.log 2>&1 &
primary_pid=$!
"$root/netns-exec" fault-replica "$bin/fault-replica" --role replica \
    --listen "10.0.1.1:$replica_port" --control-socket "$socket" \
    >/tmp/fault-replica.log 2>&1 &
replica_pid=$!
sleep 1

"$bin/fault-check" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"
tc qdisc replace dev veth-0-1 root netem delay 5ms
"$bin/fault-pending" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"
"$bin/fault-recovery" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"
tc qdisc del dev veth-0-1 root
tc qdisc replace dev veth-0-1 root netem loss 100%
if "$bin/fault-check" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"; then
    echo "fault-check unexpectedly passed during directional partition" >&2
    exit 1
else
    status=$?
    if [ "$status" -ne 1 ]; then
        echo "fault-check failed with control error $status, expected stale-read assertion" >&2
        exit "$status"
    fi
fi
tc qdisc del dev veth-0-1 root
"$bin/fault-recovery" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"
"$bin/fault-check" --primary "10.0.0.1:$primary_port" --replica-socket "$socket"
echo "fault fixture smoke: nominal, stale-read, and recovery paths passed"
