#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Scratch validation of power-loss faults on NATS JetStream; see the workflow.
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
cat >evidence/schedule.json <<'EOF'
[
  {"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0},
  {"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0},
  {"operation":{"PowerLossAll":10},"coverage_quantum":1024,"choice":0},
  {"operation":{"Restart":[0,100]},"coverage_quantum":1024,"choice":0},
  {"operation":{"Restart":[1,100]},"coverage_quantum":1024,"choice":0},
  {"operation":{"Restart":[2,100]},"coverage_quantum":1024,"choice":0},
  {"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0},
  {"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0},
  {"operation":{"Wait":1000},"coverage_quantum":1024,"choice":0}
]
EOF

step() {
    local name=$1
    shift
    echo "== $name: $*"
    local started=$SECONDS
    "$@" >"evidence/$name.log" 2>&1
    local status=$?
    echo "== $name: exit $status after $((SECONDS - started)) s"
    tail -n 40 "evidence/$name.log"
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
}

branch_search() {
    local image=$1 tag=$2
    step "$tag-run" "$harmony" debug run "$image" --config cfg.toml --seed 1 \
        --actions evidence/schedule.json --out "evidence/$tag"
    step "$tag-branch" "$harmony" branch "evidence/$tag" --step 2 --stop --out "evidence/$tag-branch"
    step "$tag-search" "$harmony" search --from "evidence/$tag-branch" --seed 11 \
        --for 75m --executions 100000000 --out "evidence/$tag-search"
    step "$tag-search-show" "$harmony" show "evidence/$tag-search"
    step "$tag-search-report" cat "evidence/$tag-search/report.json"
    if grep -q '"bug_found": true' "evidence/$tag-search/report.json"; then
        step "$tag-search-timeline" "$harmony" show "evidence/$tag-search" --finding 1 --timeline
        step "$tag-search-shrink" "$harmony" shrink "evidence/$tag-search" --finding 1 \
            --out "evidence/$tag-search-shrink" --jobs 3
        step "$tag-search-shrink-show" "$harmony" show "evidence/$tag-search-shrink"
    fi
}

case "$case_id" in
    branch-default)
        branch_search images/nats-default.oci bd
        ;;
    branch-always)
        branch_search images/nats-always.oci ba
        ;;
    hand-default)
        hand images/nats-default.oci hd
        step hd-shrink "$harmony" shrink evidence/hd --out evidence/hd-shrink --jobs 3
        step hd-shrink-show "$harmony" show evidence/hd-shrink
        ;;
    hand-always)
        hand images/nats-always.oci ha
        ;;
    search-default)
        step search "$harmony" search images/nats-default.oci --config cfg.toml --seed 7 \
            --for 2h --out evidence/sd
        step search-show "$harmony" show evidence/sd
        step search-report cat evidence/sd/report.json
        if grep -q '"bug_found": true' evidence/sd/report.json; then
            step search-shrink "$harmony" shrink evidence/sd --finding 1 \
                --out evidence/sd-shrink --jobs 3
            step search-shrink-show "$harmony" show evidence/sd-shrink
        fi
        ;;
    *)
        echo "unknown case $case_id" >&2
        exit 2
        ;;
esac
