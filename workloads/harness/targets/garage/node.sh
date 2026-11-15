#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
exec /opt/harmony/harness-net exec "${index}" -- /opt/garage/garage \
  -c "/tmp/garage/conf/n${index}.toml" server \
  >>"/tmp/garage/n${index}.log" 2>&1
