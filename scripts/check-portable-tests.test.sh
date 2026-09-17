#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A host's test count must survive the runner's coloured nextest output.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
check=${here}/check-portable-tests.sh
work=$(mktemp -d)
trap 'rm -rf "${work}"' EXIT

log=${work}/host-tests.log
printf '     Summary [ 102.250s] 1411 tests run: 1411 passed (1 slow), 25 skipped\n' >"${log}"
printf '\033[32;1m     Summary\033[0m [   5.103s] \033[1m137\033[0m tests run: \033[1m137\033[0m \033[32;1mpassed\033[0m, \033[1m0\033[0m skipped\n' >>"${log}"

test "$("${check}" "${log}" 1000)" = "portable tests run: 1548 (floor 1000)"

if "${check}" "${log}" 2000 >"${work}/floor.log" 2>&1; then
    printf 'FAIL a host below the floor passed\n'
    exit 1
fi
grep -q 'below the floor of 2000' "${work}/floor.log"

printf 'no test summary at all\n' >"${work}/empty.log"
if "${check}" "${work}/empty.log" 1 >/dev/null 2>&1; then
    printf 'FAIL a run that produced no summary passed\n'
    exit 1
fi

printf 'PASS check-portable-tests\n'
