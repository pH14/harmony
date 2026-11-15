#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
mkdir -p /tmp/summitdb/n1 /tmp/summitdb/n2 /tmp/summitdb/n3 /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
