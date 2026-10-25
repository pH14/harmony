#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"
address="10.77.0.${index}"

exec /opt/harmony/harness-net exec "${index}" -- /opt/consul/consul agent \
  -server \
  -bootstrap-expect 3 \
  -node "${name}" \
  -bind "${address}" \
  -advertise "${address}" \
  -client "${address}" \
  -data-dir "/tmp/consul/${name}" \
  -retry-join 10.77.0.1 \
  -retry-join 10.77.0.2 \
  -retry-join 10.77.0.3 \
  -config-file /opt/consul/consul.hcl \
  >>"/tmp/consul/${name}.log" 2>&1
