#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

name=${1:?missing member name}
data_dir=${2:?missing member data directory}
client_port=${3:?missing client port}
peer_port=${4:?missing peer port}
initial_cluster='member-1=http://127.0.0.1:2380,member-2=http://127.0.0.1:2382,member-3=http://127.0.0.1:2384'

exec /opt/etcd/etcd \
  --name="${name}" \
  --data-dir="${data_dir}" \
  --listen-client-urls="http://127.0.0.1:${client_port}" \
  --advertise-client-urls="http://127.0.0.1:${client_port}" \
  --listen-peer-urls="http://127.0.0.1:${peer_port}" \
  --initial-advertise-peer-urls="http://127.0.0.1:${peer_port}" \
  --initial-cluster="${initial_cluster}" \
  --initial-cluster-state=new \
  --initial-cluster-token=harmony-etcd-v35 \
  >>"/tmp/etcd/${name}.log" 2>&1
