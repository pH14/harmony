#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Node 0 of the PostgreSQL bundle.
set -u
exec setuidgid 70 "$PGROOT/bin/postgres" -D /pgmnt/pgdata
