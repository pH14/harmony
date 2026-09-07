#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Provision two Linux network namespaces and a routed pair of links. The
# root-namespace endpoint named veth-0-1 is the primary -> replica egress used
# by fault-runtime's directional tc operation; the reverse route uses
# veth-1-0. Checks read the replica through a Unix socket, so a dropped
# replication packet cannot be confused with a dropped assertion request.
set -eu
PATH=/sbin:/usr/sbin:/bin:/usr/bin
export PATH

primary_ns=fault-primary
replica_ns=fault-replica

for ns in "$primary_ns" "$replica_ns"; do
    if ip netns list | grep -Fqx "$ns"; then
        ip netns del "$ns"
    fi
done

for link in veth-0-1 veth-1-0; do
    if ip link show "$link" >/dev/null 2>&1; then
        ip link del "$link"
    fi
done

ip netns add "$primary_ns"
ip netns add "$replica_ns"

# Root-side endpoints are deliberately named by direction: traffic from the
# primary reaches the replica through root-side veth-0-1.
ip link add veth-1-0 type veth peer name p-eth
ip link set p-eth netns "$primary_ns"
ip link add veth-0-1 type veth peer name r-eth
ip link set r-eth netns "$replica_ns"

ip addr add 10.0.0.254/24 dev veth-1-0
ip addr add 10.0.1.254/24 dev veth-0-1
ip link set veth-1-0 up
ip link set veth-0-1 up

ip netns exec "$primary_ns" ip link set lo up
ip netns exec "$primary_ns" ip addr add 10.0.0.1/24 dev p-eth
ip netns exec "$primary_ns" ip link set p-eth up
ip netns exec "$primary_ns" ip route add 10.0.1.0/24 via 10.0.0.254

ip netns exec "$replica_ns" ip link set lo up
ip netns exec "$replica_ns" ip addr add 10.0.1.1/24 dev r-eth
ip netns exec "$replica_ns" ip link set r-eth up
ip netns exec "$replica_ns" ip route add 10.0.0.0/24 via 10.0.1.254

sysctl -q -w net.ipv4.ip_forward=1
