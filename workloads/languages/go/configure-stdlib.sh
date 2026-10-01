#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
sdk=${1:?usage: configure-stdlib.sh sdk-directory application-prefix evidence-directory}
application=${2:?missing application import prefix}
evidence=${3:?missing evidence directory}
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
go list std | LC_ALL=C sort -u > "$evidence/stdlib-all.txt"
(
    cd "$sdk"
    go list -deps -f '{{if .Standard}}{{.ImportPath}}{{end}}' \
        runtime github.com/antithesishq/antithesis-sdk-go/instrumentation
    grep -E '^runtime(/|$)' "$evidence/stdlib-all.txt"
) | sed '/^$/d' | LC_ALL=C sort -u > "$evidence/stdlib-excluded.txt"
LC_ALL=C comm -23 "$evidence/stdlib-all.txt" "$evidence/stdlib-excluded.txt" > "$evidence/stdlib-instrumented.txt"
test -s "$evidence/stdlib-instrumented.txt"
if grep -Eq '^runtime(/|$)' "$evidence/stdlib-instrumented.txt"; then
    echo 'runtime selected for instrumentation' >&2
    exit 1
fi
packages=$(paste -sd, "$evidence/stdlib-instrumented.txt")
printf 'export ANTITHESIS_STDLIB_PACKAGES=%q\n' "$packages"
printf 'export ANTITHESIS_INSTRUMENT=%q\n' "$application,$packages"
