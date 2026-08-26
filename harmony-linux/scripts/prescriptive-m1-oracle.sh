#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run and compare the complete ten-boot M1 Max prescriptive-time oracle.
set -euo pipefail

if [[ $# -ne 4 ]]; then
    echo "usage: $0 <signed-hvf-boot> <Image> <initramfs.cpio.gz> <output-dir>" >&2
    exit 2
fi

binary=$1
image=$2
initramfs=$3
output_dir=$4
mkdir -p "$output_dir"

baseline_oracle=
baseline_ready=
baseline_log=

for run in $(seq -w 1 10); do
    stdout="$output_dir/run-$run.stdout"
    stderr="$output_dir/run-$run.stderr"
    normalized="$output_dir/run-$run.normalized.log"
    "$binary" "$image" "$initramfs" 200000 "$normalized" >"$stdout" 2>"$stderr"

    if grep -a -q 'watchdog' "$stdout" "$stderr"; then
        echo "run $run reported a liveness watchdog" >&2
        exit 1
    fi
    oracle=$(grep -a -o 'HVF_M1_ORACLE .*' "$stdout")
    ready=$(grep -a -o 'HVF_BOOT_READY .*' "$stdout")
    if [[ -z "$oracle" || -z "$ready" ]]; then
        echo "run $run did not emit both oracle and readiness summaries" >&2
        exit 1
    fi

    if [[ -z "$baseline_oracle" ]]; then
        baseline_oracle=$oracle
        baseline_ready=$ready
        baseline_log=$normalized
    else
        [[ "$oracle" == "$baseline_oracle" ]] || {
            echo "run $run oracle summary differs" >&2
            exit 1
        }
        [[ "$ready" == "$baseline_ready" ]] || {
            echo "run $run readiness summary differs" >&2
            exit 1
        }
        cmp -s "$baseline_log" "$normalized" || {
            echo "run $run complete normalized log differs" >&2
            exit 1
        }
    fi
    echo "run=$run $oracle $ready"
done

shasum -a 256 "$image" "$initramfs"
echo "M1_TEN_RUN_ORACLE_OK normalized_logs=10 watchdogs=0"
