#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Node 0 of the etcd bundle: a single etcd member.
set -u
. /faultlab-common.sh

# A short backend batch interval makes the periodic bbolt commit — the writer
# that can persist a consistent index ahead of the data it claims to cover —
# fire often enough that a kill at a Moment can land inside the window.
exec "$ETCDROOT/etcd" \
    --name n0 \
    --data-dir /run/etcd \
    --listen-client-urls http://127.0.0.1:2379 \
    --advertise-client-urls http://127.0.0.1:2379 \
    --listen-peer-urls http://127.0.0.1:2380 \
    --initial-advertise-peer-urls http://127.0.0.1:2380 \
    --initial-cluster n0=http://127.0.0.1:2380 \
    --initial-cluster-token faultlab \
    --backend-batch-interval "$(faultlab_arg faultlab.batch_interval 1ms)" \
    --log-level error \
    --logger zap
