#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
exec /opt/etcd/etcdctl --endpoints=http://127.0.0.1:2379 endpoint health
