#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Scratch validation of disk faults on NATS JetStream; see the workflow.
set -uo pipefail
case_id=$1
harmony=$PWD/paired-cli/harmony
mkdir -p evidence
cat >cfg.toml <<'EOF'
[runner]
kind = "consonance"
backend = "kvm"
[runner.options]
ram_mib = 2048
EOF

wait_step='{"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0}'
schedule() {
    local faults=$1
    printf '[\n  %s,\n  %s,\n%s,\n  %s,\n  %s,\n  %s\n]\n' \
        "$wait_step" "$wait_step" "$faults" "$wait_step" "$wait_step" "$wait_step" >evidence/schedule.json
    cat evidence/schedule.json
}
op() {
    printf '  {"operation":{"%s":[%s,%s]},"coverage_quantum":1024,"choice":%s}' "$1" "$2" "$3" "$4"
}

step() {
    local name=$1
    shift
    echo "== $name: $*"
    local started=$SECONDS
    "$@" >"evidence/$name.log" 2>&1
    local status=$?
    echo "== $name: exit $status after $((SECONDS - started)) s"
    tail -n 60 "evidence/$name.log"
    return 0
}

hand() {
    local image=$1 tag=$2
    step "$tag-run" "$harmony" debug run "$image" --config cfg.toml --seed 1 \
        --actions evidence/schedule.json --out "evidence/$tag"
    step "$tag-repeat" "$harmony" debug run "$image" --config cfg.toml --seed 1 \
        --actions evidence/schedule.json --repeat 2 --out "evidence/$tag-repeat"
    step "$tag-show" "$harmony" show "evidence/$tag"
    step "$tag-logs" "$harmony" show "evidence/$tag" --logs
    step "$tag-timeline" "$harmony" show "evidence/$tag" --timeline
    step "$tag-repeat-show" "$harmony" show "evidence/$tag-repeat"
    step "$tag-hashes" grep -h -o '"state_hash": "[0-9a-f]*"' "evidence/$tag/report.json" \
        "evidence/$tag-repeat/report.json"
}

corrupt_all() {
    schedule "$(op DiskCorrupt 0 100 4660),
$(op DiskCorrupt 1 100 4661),
$(op DiskCorrupt 2 100 4662)"
}

case "$case_id" in
    corrupt-default)
        corrupt_all
        hand images/nats-default.oci cd
        ;;
    corrupt-always)
        corrupt_all
        hand images/nats-always.oci ca
        ;;
    error-always)
        schedule "$(op DiskError 0 300 0),
$(op Restart 0 100 0)"
        hand images/nats-always.oci ea
        ;;
    error-all-always)
        schedule "$(op DiskError 0 300 0),
$(op DiskError 1 300 0),
$(op DiskError 2 300 0)"
        hand images/nats-always.oci eaa
        ;;
    stall-always)
        schedule "$(op DiskStall 0 200 0),
$(op DiskStall 1 200 0),
$(op DiskStall 2 200 0)"
        hand images/nats-always.oci sa
        ;;
    *)
        echo "unknown case $case_id" >&2
        exit 2
        ;;
esac
