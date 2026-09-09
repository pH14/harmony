#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
exec /opt/etcd/etcd \
  --name=default \
  --data-dir=/tmp/etcd/data \
  --listen-client-urls=http://127.0.0.1:2379 \
  --advertise-client-urls=http://127.0.0.1:2379 \
  --listen-peer-urls=http://127.0.0.1:2380 \
  --initial-advertise-peer-urls=http://127.0.0.1:2380 \
  --initial-cluster=default=http://127.0.0.1:2380 \
  --initial-cluster-state=new \
  --initial-cluster-token=harmony-etcd-v35
