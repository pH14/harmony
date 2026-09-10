#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
mkdir -p \
  /tmp/etcd/data/member-1 \
  /tmp/etcd/data/member-2 \
  /tmp/etcd/data/member-3 \
  /tmp/etcd/journal
rm -f /tmp/etcd/journal/acked
rm -rf /tmp/etcd/journal/writers-started
EOF_MARKER=/tmp/etcd/journal/ready
printf 'ready\n' >"${EOF_MARKER}"
