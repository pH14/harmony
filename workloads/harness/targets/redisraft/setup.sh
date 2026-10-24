#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
mkdir -p /tmp/redisraft/n1 /tmp/redisraft/n2 /tmp/redisraft/n3 /tmp/harness/journal
rm -f /tmp/redisraft/formed
find /tmp/harness/journal -mindepth 1 -delete
