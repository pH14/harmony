#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
PATH=/bin:/sbin:/usr/bin:/usr/sbin
export PATH

/bin/busybox mount -t devtmpfs dev /dev
exec </dev/null >/dev/kmsg 2>&1
/bin/busybox mkfifo -m 0600 /dev/harmony-console
/usr/bin/mmio-console </dev/harmony-console &
HARMONY_CONSOLE_PID=$!
export HARMONY_CONSOLE_PID
# Keep the console child in PID 1's job table so shutdown can drain it.
# shellcheck source=../runtime/init.sh disable=SC1091
. /usr/lib/harmony/init >/dev/harmony-console 2>&1
