#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
# The adapter dials 127.0.0.1, so loopback ports forward to the node namespaces.
echo 1 >/proc/sys/net/ipv4/conf/all/route_localnet
for rule in "1 4001" "2 4003" "3 4005"; do
  set -- ${rule}
  spec="-d 127.0.0.1/32 -p tcp --dport $2 -j DNAT --to-destination 10.77.0.$1:$2"
  iptables -t nat -C OUTPUT ${spec} 2>/dev/null || iptables -t nat -A OUTPUT ${spec}
done
snat="-s 127.0.0.1/32 -d 10.77.0.0/24 -j SNAT --to-source 10.77.0.254"
iptables -t nat -C POSTROUTING ${snat} 2>/dev/null || iptables -t nat -A POSTROUTING ${snat}
mkdir -p /tmp/rqlite/n1 /tmp/rqlite/n2 /tmp/rqlite/n3 /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
