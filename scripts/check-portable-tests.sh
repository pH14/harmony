#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Fail when a host ran fewer portable tests than the caller's floor.
#
# A platform that stops selecting tests — a cfg that excludes a crate, a
# runner without a needed tool — otherwise reports a green run that proves
# nothing about the host.
set -euo pipefail

log="${1:?usage: check-portable-tests.sh <nextest log> <floor>}"
floor="${2:?usage: check-portable-tests.sh <nextest log> <floor>}"

total=0
while read -r count; do
    total=$((total + count))
done < <(sed $'s/\033\[[0-9;]*m//g' "$log" |
    sed -n 's/.*Summary \[[^]]*\][[:space:]]*\([0-9][0-9]*\) tests run.*/\1/p')

echo "portable tests run: $total (floor $floor)"
if [ "$total" -lt "$floor" ]; then
    echo "::error::This host ran $total portable tests, below the floor of $floor. A suite that skips its work is not host evidence."
    exit 1
fi
