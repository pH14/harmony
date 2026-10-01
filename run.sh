#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
cd "$(dirname "$0")"
gcc -O1 -Wall -o probe probe.c
gcc -O1 -Wall -o seccomp_trap seccomp_trap.c
uname -a
cat /proc/version
./probe
./seccomp_trap
