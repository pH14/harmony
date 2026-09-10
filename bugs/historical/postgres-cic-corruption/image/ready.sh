#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Exits 0 once the cluster accepts connections on the unix socket.
set -eu
exec setpriv --reuid=70 --regid=70 --clear-groups \
    /usr/lib/postgresql/bin/pg_isready -h /tmp -U postgres -d faultlab
