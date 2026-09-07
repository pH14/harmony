#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hook dispatcher for the SQLite bundle. One argument: the hook id from
# /bundle/sqlite-<version>. Directives go to stdout, diagnostics to stderr.
set -u
. /faultlab-common.sh

case "$1" in
    # One large commit grows the WAL past the size the last checkpoint saw,
    # which the bug needs before a later checkpoint skips frames.
    1) exec "$SQLITEROOT/faultlab-sqlite" /run/fl.db burst "$(faultlab_arg faultlab.burst 2000)" /run/journal ;;
    2) exec "$SQLITEROOT/faultlab-sqlite" /run/fl.db verify /run/journal ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
