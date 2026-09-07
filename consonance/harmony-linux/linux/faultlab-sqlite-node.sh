#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Node 0 of the SQLite bundle: the checkpointer. It checkpoints without pause,
# the way the upstream report's backup pipeline did, so a checkpoint is
# always in progress for a writer's WAL reset to land in.
set -u
exec "$SQLITEROOT/faultlab-sqlite" /run/fl.db checkpointer
