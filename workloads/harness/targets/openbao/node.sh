#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
index=${1:?missing node index}
exec /opt/harmony/harness-net exec "${index}" -- /opt/bao/bao server \
  -config="/tmp/bao/conf/n${index}.hcl" \
  >>"/tmp/bao/n${index}.log" 2>&1
