#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
/opt/harmony/harness-net setup 3
mkdir -p /tmp/garage/n1 /tmp/garage/n2 /tmp/garage/n3 /tmp/garage/conf /tmp/harness/journal
find /tmp/harness/journal -mindepth 1 -delete
rm -f /tmp/garage/formed
for i in 1 2 3; do
  sed "s/@INDEX@/${i}/g" /opt/garage/garage.toml.in > "/tmp/garage/conf/n${i}.toml"
  chmod 0600 "/tmp/garage/conf/n${i}.toml"
done
