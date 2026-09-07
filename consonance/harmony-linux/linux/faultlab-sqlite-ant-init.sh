#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# rdinit for the SQLite WAL-reset bundle as Antithesis ran it
# (bugs/historical/sqlite-wal-reset): SQLite with their reach markers, two
# identical writer processes, coverage-counted edges for pauses.
# FAULTLAB_SQLITEVER selects the release: 3.51.2 carries the bug, 3.51.3 is
# the fixed control.
set -u
. /faultlab-common.sh

: "${FAULTLAB_SQLITEVER:=3.51.2}"
export FAULTLAB_SQLITEVER
ANTROOT=/opt/ant-sqlite-$FAULTLAB_SQLITEVER
export ANTROOT

faultlab_mount_base
"$ANTROOT/faultlab-sqlite-ant" /run/fl.db init

FAULTLAB_NODE="/w/ant-sqlite-node.sh 0"
FAULTLAB_NODE2="/w/ant-sqlite-node.sh 1"
FAULTLAB_READY=/w/ant-sqlite-ready.sh
export FAULTLAB_NODE FAULTLAB_NODE2 FAULTLAB_READY

hooks=$(faultlab_arg faultlab.hooks "")
if [ -n "$hooks" ]; then
    # shellcheck disable=SC2046  # the hook list is a deliberate word split
    faultlab_control /w/ant-sqlite-hooks.sh $(echo "$hooks" | tr ',' ' ')
fi
faultlab_agent /bundle/ant-sqlite
