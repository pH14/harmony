#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Ready check for the PostgreSQL bundle: exits 0 once the cluster accepts
# connections on the unix socket.
exec setuidgid 70 "$PGROOT/bin/pg_isready" -h /tmp -U postgres -d faultlab
