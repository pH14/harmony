#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Ready check for the Antithesis SQLite bundle: exits 0 once the schema exists.
set -u
exec "$ANTROOT/faultlab-sqlite-ant" /run/fl.db ready
