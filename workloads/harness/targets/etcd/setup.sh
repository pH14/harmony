#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
members=${1:?missing member count}
[ "${members}" -eq 1 ] || /opt/harmony/harness-net setup "${members}"
mkdir -p /tmp/etcd /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
