#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Hook dispatcher for the Antithesis SQLite bundle. One argument: the hook id
# from /bundle/ant-sqlite. Directives go to stdout, diagnostics to stderr.
set -u
. /faultlab-common.sh

case "$1" in
    1) exec "$ANTROOT/faultlab-sqlite-ant" /run/fl.db verify ;;
    *) echo "unknown hook $1" >&2; exit 1 ;;
esac
