#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

# Forms the cluster once per execution: n1 connects to n2 and n3, assigns all
# three a role in one zone, applies layout version 1, then imports the
# harness key and creates its bucket. Each step can run again after a
# probe that failed part way.
garage() { /opt/garage/garage -c /tmp/garage/conf/n1.toml "$@"; }

if [ ! -e /tmp/garage/formed ]; then
  ids=""
  for i in 1 2 3; do
    id=$(/opt/garage/garage -c "/tmp/garage/conf/n${i}.toml" node id -q)
    ids="${ids} ${id%%@*}"
    [ "${i}" = 1 ] || garage node connect "${id}" >/dev/null
  done
  if ! garage layout show | grep -q "Current cluster layout version: [1-9]"; then
    # shellcheck disable=SC2086
    garage layout assign -z dc1 -c 100M ${ids} >/dev/null
    garage layout apply --version 1 >/dev/null
  fi
  garage key info GK0123456789abcdef01234567 >/dev/null 2>&1 \
    || garage key import --yes -n harness GK0123456789abcdef01234567 \
       0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef >/dev/null
  garage bucket info harness >/dev/null 2>&1 || garage bucket create harness >/dev/null
  garage bucket allow --read --write --owner harness --key GK0123456789abcdef01234567 >/dev/null
  touch /tmp/garage/formed
fi
exec /opt/harmony/garage-harness ready
