#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Ready check for the etcd bundle: exits 0 once the member serves clients.
exec /opt/etcd/etcdctl --endpoints=127.0.0.1:2379 endpoint health
