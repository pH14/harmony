#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
mkdir -p /tmp/dqlite/n1 /tmp/dqlite/n2 /tmp/dqlite/n3 /tmp/harness/journal
rm -f /tmp/dqlite/formed
find /tmp/harness/journal -mindepth 1 -delete
