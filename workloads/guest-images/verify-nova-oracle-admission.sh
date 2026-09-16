#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

[ "$#" -eq 6 ] || {
    echo "usage: $0 ORACLE KERNEL PLATFORM OCI ROM NEW_REPORT_DIRECTORY" >&2
    exit 2
}
[ "${HARMONY_CONSONANCE_RESTORE_ORACLE:-}" = 1 ] || {
    echo "FAIL: Nova admission requires HARMONY_CONSONANCE_RESTORE_ORACLE=1" >&2
    exit 1
}
[ "${HARMONY_CONSONANCE_ORACLE_TREE_SEED+x}" != x ] || {
    echo "FAIL: Nova admission forbids a tree-seed override" >&2
    exit 1
}
here=$(cd "$(dirname "$0")" && pwd)
oracle=$1
kernel=$2
platform=$3
oci=$4
rom=$5
report=$6
[ ! -e "$report" ] || { echo "FAIL: report directory exists" >&2; exit 1; }
mkdir -p "$report/dump" "$report/verification"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
status=0
if ! "$oracle" --prepare-admission "$kernel" "$platform" "$oci" "$rom" "$work/dump" \
    >"$report/dump.log" 2>&1; then
    status=1
elif ! python3 "$here/verify-prepared-admission.py" verify "$work/dump" \
    --output "$work/verification" \
    --baseline "$here/admission/nova-oracle-composition.json" \
    >"$report/verification.log" 2>&1; then
    status=1
fi
for stage in dump verification; do
    for metadata in "$work/$stage/"*.json; do
        [ ! -f "$metadata" ] || cp "$metadata" "$report/$stage/"
    done
done
if [ "$status" -ne 0 ]; then
    cat "$report/dump.log" >&2
    [ ! -f "$report/verification.log" ] || cat "$report/verification.log" >&2
    echo "FAIL: Nova oracle admission did not pass; execution is forbidden" >&2
fi
exit "$status"
