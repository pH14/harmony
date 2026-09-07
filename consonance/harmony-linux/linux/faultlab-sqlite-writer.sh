#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Node 1 of the SQLite bundle: the writer. Each commit journals the largest id
# SQLite acknowledged, which is what the verify hook reads back.
set -u
. /faultlab-common.sh
exec "$SQLITEROOT/faultlab-sqlite" /run/fl.db writer "$(faultlab_arg faultlab.rows 20)" "$(faultlab_arg faultlab.big_rows 2000)" "$(faultlab_arg faultlab.gap_us 500)" /run/journal
