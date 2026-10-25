#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
index=${1:?missing node index}
export HOME=/tmp/minio/home
export MINIO_ROOT_USER=harness
export MINIO_ROOT_PASSWORD=harness-secret-0123456789
export MINIO_BROWSER=off
exec /opt/harmony/harness-net exec "${index}" -- /opt/minio/minio server \
  --address "10.77.0.${index}:9000" \
  --certs-dir "/tmp/minio/home/certs" \
  http://10.77.0.1:9000/tmp/minio/n1 \
  http://10.77.0.2:9000/tmp/minio/n2 \
  http://10.77.0.3:9000/tmp/minio/n3 \
  http://10.77.0.4:9000/tmp/minio/n4 \
  >>"/tmp/minio/n${index}.log" 2>&1
