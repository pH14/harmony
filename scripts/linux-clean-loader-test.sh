#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
for name in $(env | sed -n 's/^\(LD_[A-Za-z0-9_]*\)=.*/\1/p'); do
    unset "$name"
done
unset GLIBC_TUNABLES
exec "$@"
