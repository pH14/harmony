#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# rdinit for the PostgreSQL CREATE INDEX CONCURRENTLY bundle
# (bugs/historical/postgres-cic-corruption). FAULTLAB_PGVER selects the tree:
# 14.3 carries the bug, 14.4 is the fixed control arm.
set -u
. /faultlab-common.sh

: "${FAULTLAB_PGVER:=14.3}"
export FAULTLAB_PGVER
PGROOT=/usr/lib/postgresql/$FAULTLAB_PGVER
export PGROOT

faultlab_mount_base
mkdir -p /pgmnt
# The cluster is initdb'd at build time into a fixed-UUID ext4 file, so every
# run starts from the same on-disk bytes down to the system identifier.
# Loop devices are allocated on demand through /dev/loop-control, so no
# /dev/loopN node exists to name here; mount does the allocation itself.
mount -o loop -t ext4 "/pgdata-$FAULTLAB_PGVER.ext4" /pgmnt \
    || faultlab_say "mount of /pgdata-$FAULTLAB_PGVER.ext4 failed"
chown -R 70:70 /pgmnt/pgdata 2>/dev/null

FAULTLAB_NODE=/w/pg-node.sh
FAULTLAB_READY=/w/pg-ready.sh
export FAULTLAB_NODE FAULTLAB_READY

hooks=$(faultlab_arg faultlab.hooks "")
if [ -n "$hooks" ]; then
    # shellcheck disable=SC2046  # the hook list is a deliberate word split
    faultlab_control /w/pg-hooks.sh $(echo "$hooks" | tr ',' ' ')
fi
faultlab_agent "/bundle/pgcic-$FAULTLAB_PGVER"
