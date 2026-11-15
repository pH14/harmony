#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu

index=${1:?missing node index}
name="n${index}"
address="10.77.0.${index}"

exec /opt/harmony/harness-net exec "${index}" -- /opt/nomad/nomad agent \
  -config /opt/nomad/nomad.hcl \
  -node "${name}" \
  -bind "${address}" \
  -data-dir "/tmp/nomad/${name}" \
  -retry-join 10.77.0.1 \
  -retry-join 10.77.0.2 \
  -retry-join 10.77.0.3 \
  >>"/tmp/nomad/${name}.log" 2>&1
