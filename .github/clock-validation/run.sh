#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Scratch triage of the NATS key-value clock-fault finding; see the workflow.
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
harmony=$PWD/paired-cli/harmony
mkdir -p evidence
cat >cfg.toml <<'EOF'
[runner]
kind = "consonance"
backend = "kvm"
[runner.options]
ram_mib = 2048
EOF

clock_operation() {
    case "$1" in
        stopped) echo '{"Clock":{"node":1,"offset_ms":0,"rate_ppm":-1000000,"ticks":128}}' ;;
        rate50) echo '{"Clock":{"node":1,"offset_ms":0,"rate_ppm":-500000,"ticks":128}}' ;;
        rate10) echo '{"Clock":{"node":1,"offset_ms":0,"rate_ppm":-100000,"ticks":128}}' ;;
        rate1) echo '{"Clock":{"node":1,"offset_ms":0,"rate_ppm":-10000,"ticks":128}}' ;;
        offset1s) echo '{"Clock":{"node":1,"offset_ms":-1000,"rate_ppm":0,"ticks":128}}' ;;
        offset10s) echo '{"Clock":{"node":1,"offset_ms":-10000,"rate_ppm":0,"ticks":128}}' ;;
        noclock) echo '{"Wait":128}' ;;
        *)
            echo "unknown variant $1" >&2
            exit 2
            ;;
    esac
}

actions() {
    python3 -I - "$here/shrunk.json" "$(clock_operation "$1")" "$2" <<'EOF'
import json, random, sys
actions = json.load(open(sys.argv[1]))
actions[1]["operation"] = json.loads(sys.argv[2])
draw = int(sys.argv[3])
if draw:
    rand = random.Random(draw)
    for action in actions:
        action["choice"] = rand.getrandbits(64)
print(json.dumps(actions, indent=1))
EOF
}

summarize() {
    python3 -I - "$@" <<'EOF'
import json, sys
case, draw, report = sys.argv[1:4]
try:
    replay = json.load(open(report))["replays"][0]
    print("\t".join([case, draw, "violation" if replay["bug"] else "clean",
                     replay["state_hash"][:16], "; ".join(replay["violations"])]))
except Exception as error:
    print("\t".join([case, draw, "error", "", str(error)]))
EOF
}

one() {
    local image=$1 variant=$2 draw=$3
    local tag="$variant-$draw"
    actions "$variant" "$draw" >"evidence/$tag-actions.json"
    "$harmony" debug run "$image" --config cfg.toml --seed 7 \
        --actions "evidence/$tag-actions.json" --out "evidence/$tag" \
        >"evidence/$tag.log" 2>&1
    summarize "$variant" "$draw" "evidence/$tag/report.json" >"evidence/$tag.tsv"
    cat "evidence/$tag.tsv"
}

step() {
    local name=$1
    shift
    echo "== $name: $*"
    local started=$SECONDS
    "$@" >"evidence/$name.log" 2>&1
    local status=$?
    echo "== $name: exit $status after $((SECONDS - started)) s"
    tail -n 80 "evidence/$name.log"
    return 0
}

case "$1" in
    one)
        one "$2" "$3" "$4"
        ;;
    variant)
        image=$2 variant=$3 draws=$4
        seq 0 "$draws" | xargs -P 2 -I{} bash "$0" one "$image" "$variant" {}
        cat evidence/"$variant"-*.tsv | sort -t "$(printf '\t')" -k2,2n >evidence/summary.tsv
        echo "== summary"
        cat evidence/summary.tsv
        ;;
    explain)
        image=$2
        actions stopped 0 >evidence/recording-actions.json
        step recording "$harmony" debug run "$image" --config cfg.toml --seed 7 \
            --actions evidence/recording-actions.json --out evidence/recording
        summarize recording 0 evidence/recording/report.json | tee evidence/summary.tsv
        if grep -q violation evidence/summary.tsv; then
            step explain "$harmony" explain evidence/recording --no-shrink --jobs 3 \
                --log '/tmp/nats/*.log' \
                --data /tmp/nats/n1 --data /tmp/nats/n2 --data /tmp/nats/n3 \
                --history 'ANTITHESIS_OUTPUT_DIR= /opt/harmony/nats-kv-harness check' \
                --out evidence/explain
            step explain-md cat evidence/explain/explain.md
            step timeline "$harmony" show evidence/explain --timeline
        fi
        ;;
    *)
        echo "unknown case $1" >&2
        exit 2
        ;;
esac
