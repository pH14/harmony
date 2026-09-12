#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Node 0: the seeded cluster. PostgreSQL refuses to run as root, so the
# postmaster drops to the uid that owns the baked data directory.
set -eu
exec setpriv --reuid=70 --regid=70 --clear-groups \
    /usr/lib/postgresql/bin/postgres -D /var/lib/postgresql/data
