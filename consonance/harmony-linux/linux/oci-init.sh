#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The kernel always enters through this stable PID 1 path. The platform init
# owns guest setup and invokes the platform supervisor and pinned OCI runtime.
PATH=/bin:/sbin:/usr/bin:/usr/sbin
export PATH
exec /usr/lib/harmony/init "$@"
