#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
endpoints='http://127.0.0.1:2379,http://127.0.0.1:2381,http://127.0.0.1:2383'
exec env ETCDCTL_API=3 /opt/etcd/etcdctl --endpoints="${endpoints}" endpoint health
