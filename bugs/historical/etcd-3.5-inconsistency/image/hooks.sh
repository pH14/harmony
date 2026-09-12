#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

journal=/tmp/etcd/journal/acked
verified=/tmp/etcd/journal/verified
endpoints="http://127.0.0.1:2379 http://127.0.0.1:2381 http://127.0.0.1:2383"

# Both checks run inside one process. The guest has a single processor and
# shares it with the three members under test, so a check that spawned a
# process per step would take the processor exactly when a restarted member
# needs it to catch up.
case "$1" in
  2)
    # The window check: verify what has been acknowledged since the last
    # passing check, reading only the journal bytes appended since then. Hook 3
    # catches what a window stepped over.
    exec /opt/harmony/etcd-oracle window "${journal}" "${verified}" ${endpoints}
    ;;
  3)
    # The full sweep: compare the entire journal against every member. Run once
    # at the end of a measurement, so a loss no window covered is still
    # reported.
    exec /opt/harmony/etcd-oracle sweep "${journal}" ${endpoints}
    ;;
  *)
    echo "unknown hook $1" >&2
    exit 2
    ;;
esac
