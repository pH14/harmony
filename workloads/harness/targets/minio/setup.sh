#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 4
mkdir -p /tmp/minio/n1 /tmp/minio/n2 /tmp/minio/n3 /tmp/minio/n4 /tmp/minio/home /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
