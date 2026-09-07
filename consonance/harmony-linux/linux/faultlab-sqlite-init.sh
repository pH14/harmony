#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# rdinit for the SQLite WAL-reset bundle (bugs/historical/sqlite-wal-reset).
# FAULTLAB_SQLITEVER selects the release: 3.51.2 carries the bug, 3.51.3 is
# the fixed control arm.
set -u
. /faultlab-common.sh

: "${FAULTLAB_SQLITEVER:=3.51.2}"
export FAULTLAB_SQLITEVER
SQLITEROOT=/opt/sqlite-$FAULTLAB_SQLITEVER
export SQLITEROOT

faultlab_mount_base
# The database and its WAL live on tmpfs: the bug is a race over the shared
# wal-index, and nothing in it depends on what reaches a disk.
"$SQLITEROOT/faultlab-sqlite" /run/fl.db init

FAULTLAB_NODE=/w/sqlite-node.sh
FAULTLAB_NODE2=/w/sqlite-writer.sh
FAULTLAB_READY=/w/sqlite-ready.sh
export FAULTLAB_NODE FAULTLAB_NODE2 FAULTLAB_READY

hooks=$(faultlab_arg faultlab.hooks "")
if [ -n "$hooks" ]; then
    # shellcheck disable=SC2046  # the hook list is a deliberate word split
    faultlab_control /w/sqlite-hooks.sh $(echo "$hooks" | tr ',' ' ')
fi
faultlab_agent "/bundle/sqlite-$FAULTLAB_SQLITEVER"
