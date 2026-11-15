#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
mkdir -p /tmp/bao/n1 /tmp/bao/n2 /tmp/bao/n3 /tmp/bao/conf /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
rm -f /tmp/bao/token /tmp/bao/token.new /tmp/bao/init.json /tmp/bao/init.json.new /tmp/bao/mounted
for i in 1 2 3; do
  sed "s/@INDEX@/${i}/g" /opt/bao/bao.hcl.in > "/tmp/bao/conf/n${i}.hcl"
done
