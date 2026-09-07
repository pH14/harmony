#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# One writer of the Antithesis SQLite bundle. The argument is its writer id.
# faultlab.edge<id> names the edge at which it pauses (0: never),
# faultlab.hdr<id> the checkpoint header read that finds the WAL fully
# backfilled at which it pauses instead (0: never), faultlab.edge_sleep_us
# how long; faultlab.seed seeds its draws;
# faultlab.reach_all logs every marker passage; faultlab.jitter_log logs
# every random pause with its distance past the latest header read.
set -u
. /faultlab-common.sh
W=$1
FAULTLAB_EDGE=$(faultlab_arg "faultlab.edge$W" 0)
FAULTLAB_PAUSE_AT="checkpoint: header shows WAL fully backfilled"
FAULTLAB_PAUSE_K=$(faultlab_arg "faultlab.hdr$W" 0)
FAULTLAB_EDGE_SLEEP_US=$(faultlab_arg faultlab.edge_sleep_us 50000)
export FAULTLAB_EDGE FAULTLAB_PAUSE_AT FAULTLAB_PAUSE_K FAULTLAB_EDGE_SLEEP_US
if [ "$(faultlab_arg faultlab.reach_all 0)" != 0 ]; then
    FAULTLAB_REACH_ALL=1
    export FAULTLAB_REACH_ALL
fi
if [ "$(faultlab_arg faultlab.jitter_log 0)" != 0 ]; then
    FAULTLAB_JITTER_LOG=1
    export FAULTLAB_JITTER_LOG
fi
exec "$ANTROOT/faultlab-sqlite-ant" /run/fl.db run "$W" "$(faultlab_arg faultlab.seed 1)"
