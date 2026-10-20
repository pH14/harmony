#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Scratch validation of clock faults on NATS JetStream key-value; see the workflow.
set -uo pipefail
case_id=$1
harmony=$PWD/paired-cli/harmony
image=$PWD/images/nats-kv-clock.oci
mkdir -p evidence
cat >cfg.toml <<'EOF'
[runner]
kind = "consonance"
backend = "kvm"
[runner.options]
ram_mib = 2048
EOF

action() {
    printf '  {"operation":%s,"coverage_quantum":1024,"choice":%s}' "$1" "${2:-0}"
}

schedule() {
    local clock=$1
    local shift_one shift_stop shift_fast shift_back
    if [ "$clock" = yes ]; then
        shift_one='{"Clock":{"node":0,"offset_ms":3600000,"rate_ppm":0,"ticks":500}}'
        shift_stop='{"Clock":{"node":1,"offset_ms":0,"rate_ppm":-1000000,"ticks":500}}'
        shift_fast='{"ClockAll":{"offset_ms":0,"rate_ppm":4000000,"ticks":500}}'
        shift_back='{"ClockAll":{"offset_ms":-10000,"rate_ppm":0,"ticks":300}}'
    else
        shift_one='{"Wait":500}'
        shift_stop='{"Wait":500}'
        shift_fast='{"Wait":500}'
        shift_back='{"Wait":300}'
    fi
    {
        echo '['
        action '{"Wait":1000}'; echo ,
        action '{"Wait":1000}'; echo ,
        action "$shift_one"; echo ,
        action '{"Wait":300}'; echo ,
        action "$shift_stop"; echo ,
        action '{"Wait":300}'; echo ,
        action "$shift_fast" 24301; echo ,
        action '{"Wait":300}'; echo ,
        action "$shift_back"; echo ,
        action '{"Wait":1000}'; echo ,
        action '{"Wait":1000}'; echo
        echo ']'
    }
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

probe='for n in 1 2 3; do echo "== n$n clock probe"; cat /tmp/nats/n$n.clock; echo "== n$n nats log"; cat /tmp/nats/n$n.log; done'

hand() {
    local tag=$1 clock=$2
    schedule "$clock" >"evidence/$tag-schedule.json"
    cat "evidence/$tag-schedule.json"
    step "$tag-run" "$harmony" debug run "$image" --config cfg.toml --seed 1 \
        --actions "evidence/$tag-schedule.json" --out "evidence/$tag"
    step "$tag-repeat" "$harmony" debug run "$image" --config cfg.toml --seed 1 \
        --actions "evidence/$tag-schedule.json" --repeat 2 --out "evidence/$tag-repeat"
    step "$tag-show" "$harmony" show "evidence/$tag"
    step "$tag-timeline" "$harmony" show "evidence/$tag" --timeline
    step "$tag-logs" "$harmony" show "evidence/$tag" --logs
    step "$tag-report" cat "evidence/$tag/report.json"
    step "$tag-repeat-report" cat "evidence/$tag-repeat/report.json"
    step "$tag-probe" "$harmony" branch "evidence/$tag" --step 11 --stop \
        --exec "$probe" --out "evidence/$tag-probe"
}

case "$case_id" in
    hand)
        hand hand yes
        ;;
    baseline)
        hand baseline no
        ;;
    search)
        step search "$harmony" search "$image" --config cfg.toml --seed 7 \
            --for 60m --out evidence/search
        step search-show "$harmony" show evidence/search
        step search-report cat evidence/search/report.json
        if grep -q '"bug_found": true' evidence/search/report.json; then
            step search-timeline "$harmony" show evidence/search --finding 1 --timeline
            step search-shrink "$harmony" shrink evidence/search --finding 1 \
                --out evidence/search-shrink --jobs 3
            step search-shrink-show "$harmony" show evidence/search-shrink
            step search-shrink-timeline "$harmony" show evidence/search-shrink --timeline
        fi
        ;;
    *)
        echo "unknown case $case_id" >&2
        exit 2
        ;;
esac
