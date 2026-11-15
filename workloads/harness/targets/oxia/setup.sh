#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 4
mkdir -p /tmp/oxia/n1 /tmp/oxia/n2 /tmp/oxia/n3 /tmp/oxia/c /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
