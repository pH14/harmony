#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
usage='usage: configure-instrumentation.sh sdk-directory evidence-directory [package...]'
sdk=${1:?$usage}
evidence=${2:?$usage}
shift 2
if [ "$#" -eq 0 ]; then
    set -- .
fi
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
sdk_package=github.com/antithesishq/antithesis-sdk-go/instrumentation
module_template='{{if not .Standard}}{{with .Module}}{{.Path}}{{else}}{{.ImportPath}}{{end}}{{end}}'
go list std | LC_ALL=C sort -u > "$evidence/stdlib-all.txt"
(
    cd "$sdk"
    GO111MODULE=on go list -deps -f '{{if .Standard}}{{.ImportPath}}{{end}}' runtime "$sdk_package"
    grep -E '^runtime(/|$)' "$evidence/stdlib-all.txt"
) | sed '/^$/d' | LC_ALL=C sort -u > "$evidence/stdlib-excluded.txt"
LC_ALL=C comm -23 "$evidence/stdlib-all.txt" "$evidence/stdlib-excluded.txt" > "$evidence/stdlib-instrumented.txt"
test -s "$evidence/stdlib-instrumented.txt"
if grep -Eq '^runtime(/|$)' "$evidence/stdlib-instrumented.txt"; then
    echo 'runtime selected for instrumentation' >&2
    exit 1
fi
(
    cd "$sdk"
    echo github.com/antithesishq/antithesis-sdk-go
    GO111MODULE=on go list -deps -f "$module_template" "$sdk_package"
) | sed '/^$/d' | LC_ALL=C sort -u > "$evidence/modules-excluded.txt"
go list -deps -f "$module_template" "$@" | sed '/^$/d' | LC_ALL=C sort -u > "$evidence/modules-all.txt"
LC_ALL=C comm -23 "$evidence/modules-all.txt" "$evidence/modules-excluded.txt" > "$evidence/modules-instrumented.txt"
if ! test -s "$evidence/modules-instrumented.txt"; then
    echo 'no module selected for instrumentation' >&2
    exit 1
fi
packages=$(paste -sd, "$evidence/stdlib-instrumented.txt")
modules=$(paste -sd, "$evidence/modules-instrumented.txt")
printf 'export ANTITHESIS_STDLIB_PACKAGES=%q\n' "$packages"
printf 'export ANTITHESIS_INSTRUMENT=%q\n' "$modules,$packages"
