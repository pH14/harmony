#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Ready check for the SQLite bundle: exits 0 once the table answers.
set -u
exec "$SQLITEROOT/faultlab-sqlite" /run/fl.db ready
