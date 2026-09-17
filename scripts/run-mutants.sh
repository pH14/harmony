#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run cargo-mutants and retry only when the run itself failed. A surviving or
# timed-out mutant is a real result and must not be retried away.
set -uo pipefail

if [[ $# -lt 1 ]]; then
    printf 'usage: run-mutants.sh <output-directory> [cargo-mutants arguments...]\n' >&2
    exit 2
fi

output=$1
shift
mkdir -p "${output}"

for attempt in 1 2; do
    cargo mutants --output "${output}" "$@"
    rc=$?
    if [[ ${rc} -eq 0 ]]; then
        exit 0
    fi
    if [[ ${rc} -eq 2 || ${rc} -eq 3 ]]; then
        printf '::error::cargo-mutants reported surviving or timed-out mutants (exit %s)\n' "${rc}"
        exit "${rc}"
    fi
    printf '::warning::cargo-mutants could not complete (exit %s), attempt %s of 2\n' "${rc}" "${attempt}"
    sleep 15
done

printf '::error::cargo-mutants could not complete after two attempts (exit %s)\n' "${rc}"
exit "${rc}"
