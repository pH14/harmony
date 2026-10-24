#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

# Initializes the cluster once per execution through n1, which bootstraps the
# Raft cluster; n2 and n3 join through retry_join and unseal themselves with
# the static seal. Then mounts KV version 2 at secret/.
bao() { BAO_ADDR=http://10.77.0.1:8200 /opt/bao/bao "$@"; }

if [ ! -s /tmp/bao/token ]; then
  if ! grep -q root_token /tmp/bao/init.json 2>/dev/null; then
    bao operator init -recovery-shares=1 -recovery-threshold=1 -format=json > /tmp/bao/init.json.new
    mv /tmp/bao/init.json.new /tmp/bao/init.json
  fi
  sed -n 's/.*"root_token": *"\([^"]*\)".*/\1/p' /tmp/bao/init.json > /tmp/bao/token.new
  test -s /tmp/bao/token.new
  mv /tmp/bao/token.new /tmp/bao/token
fi
if [ ! -e /tmp/bao/mounted ]; then
  export BAO_TOKEN="$(cat /tmp/bao/token)"
  bao secrets list -format=json | grep -q '"secret/"' || bao secrets enable -path=secret kv-v2 >/dev/null
  touch /tmp/bao/mounted
fi
exec /opt/harmony/bao-harness ready
