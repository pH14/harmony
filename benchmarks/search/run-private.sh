#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run a registered licensed-ROM panel entirely on the operator's private host.
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
  echo "usage: $0 ASSETS_JSON OUTPUT_DIRECTORY [MANIFEST]" >&2
  exit 2
fi

assets=$1
output=$2
manifest=${3:-benchmarks/search/evaluation.json}
case "$manifest" in
  benchmarks/search/evaluation.json|benchmarks/search/smb-reference.json) ;;
  *) echo "manifest must be evaluation.json or smb-reference.json" >&2; exit 2 ;;
esac
test -f "$assets" || { echo "asset inventory does not exist: $assets" >&2; exit 2; }

build_dir=$(mktemp -d "${TMPDIR:-/tmp}/harmony-nes-search-build.XXXXXX")
export_dir="${output}-public"
cleanup() { rm -rf "$build_dir"; }
trap cleanup EXIT

build_jobs=${HARMONY_SEARCH_BUILD_JOBS:-24}
jobs=${HARMONY_SEARCH_JOBS:-3}
cpus=${HARMONY_SEARCH_CPUS:-24}
memory=${HARMONY_SEARCH_MEMORY_MIB:-40000}
finish=${HARMONY_SEARCH_FINISH_SECONDS:-600}

python3 benchmarks/search/eval.py build --out "$build_dir" --jobs "$build_jobs"
run_status=0
python3 benchmarks/search/eval.py run "$manifest" \
  --assets "$assets" \
  --binary "$build_dir/nes-eval" \
  --build-info "$build_dir/build-info.json" \
  --out "$output" --jobs "$jobs" --cpus "$cpus" \
  --memory-capacity-mib "$memory" --finish-seconds "$finish" || run_status=$?

export_status=0
if [[ -f "$output/results.json" ]]; then
  python3 benchmarks/search/eval.py export "$output" --out "$export_dir" || export_status=$?
else
  echo 'The common runner produced no matrix index.' >&2
  export_status=1
fi

if (( run_status != 0 )); then
  exit "$run_status"
fi
exit "$export_status"
