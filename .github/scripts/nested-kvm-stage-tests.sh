#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the full-VMM and backend test binaries that the nested boot harness
# stages into its disposable initramfs. This script never runs the tests.
set -Eeuo pipefail

fail() {
    printf 'nested-kvm-stage-tests: %s\n' "$*" >&2
    exit 1
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || fail "required tool is unavailable: $1"
}

if [[ "$#" -ne 1 || -z "${1:-}" ]]; then
    printf 'usage: %s <staging-root>\n' "$0" >&2
    exit 2
fi

for tool in cargo chmod cp grep mkdir python3 readelf realpath sha256sum; do
    require_tool "$tool"
done

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(realpath -e -- "$script_dir/../..")
staging_root=$(realpath -e -- "$1") || fail "staging root does not exist: $1"
[[ -d "$staging_root" ]] || fail "staging root is not a directory: $staging_root"
[[ -w "$staging_root" ]] || fail "staging root is not writable: $staging_root"
[[ "$staging_root" != / ]] || fail "refusing to use / as the staging root"
if [[ "$staging_root" == "$repo_root" || "$staging_root" == "$repo_root/"* || \
    "$repo_root" == "$staging_root/"* ]]; then
    fail "staging root must be separate from the repository: $staging_root"
fi

reports_dir="$repo_root/reports"
if [[ -L "$reports_dir" || (-e "$reports_dir" && ! -d "$reports_dir") ]]; then
    fail "reports path is not a directory: $reports_dir"
fi
mkdir -p -- "$reports_dir"
build_json="$reports_dir/nested-snapshot-build.json"
[[ ! -e "$build_json" && ! -L "$build_json" ]] || \
    fail "build report already exists: $build_json"

cd -- "$repo_root"
RUSTFLAGS='-C target-feature=+crt-static' CARGO_BUILD_JOBS=4 \
    cargo test --locked --no-run --target x86_64-unknown-linux-gnu \
    -p vmm-core --test x86_cpu_snapshots \
    --message-format=json-render-diagnostics >"$build_json"

select_test_executable() {
    local cargo_json=$1
    local target_name=$2
    python3 - "$cargo_json" "$target_name" <<'PY'
import json
import sys

path = sys.argv[1]
target_name = sys.argv[2]
matches = []
with open(path, encoding="utf-8") as stream:
    for line_number, line in enumerate(stream, 1):
        if not line.strip():
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as error:
            raise SystemExit(f"cargo JSON line {line_number} is invalid: {error}")
        if message.get("reason") != "compiler-artifact":
            continue
        target = message.get("target")
        if not isinstance(target, dict) or target.get("name") != target_name:
            continue
        kinds = target.get("kind")
        if not isinstance(kinds, list) or "test" not in kinds:
            continue
        executable = message.get("executable")
        if not isinstance(executable, str) or not executable:
            raise SystemExit(f"matching {target_name} artifact has no executable")
        matches.append(executable)

unique = list(dict.fromkeys(matches))
if len(unique) != 1:
    if not unique:
        raise SystemExit(f"cargo produced no {target_name} test executable")
    raise SystemExit(
        f"cargo produced multiple {target_name} test executables: "
        + ", ".join(unique)
    )
print(unique[0])
PY
}

validate_static_executable() {
    local test_executable=$1
    local label=$2
    [[ -f "$test_executable" ]] || fail "$label executable is not a regular file: $test_executable"
    [[ -x "$test_executable" ]] || fail "$label executable is not executable: $test_executable"
    local program_headers
    program_headers=$(readelf -l -- "$test_executable") || \
        fail "cannot inspect $label executable: $test_executable"
    if printf '%s\n' "$program_headers" | grep -Eq '^[[:space:]]*INTERP([[:space:]]|$)'; then
        fail "$label executable is dynamically linked: $test_executable"
    fi
}

test_executable=$(select_test_executable "$build_json" x86_cpu_snapshots)
validate_static_executable "$test_executable" x86_cpu_snapshots

bin_dir="$staging_root/bin"
if [[ -L "$bin_dir" || (-e "$bin_dir" && ! -d "$bin_dir") ]]; then
    fail "staging bin path is not a directory: $bin_dir"
fi
mkdir -p -- "$bin_dir"
stage_test_executable() {
    local test_executable=$1
    local staged=$2
    local digest_report=$3
    [[ ! -e "$staged" && ! -L "$staged" ]] || \
        fail "staging destination already exists: $staged"
    cp -- "$test_executable" "$staged"
    chmod 0555 -- "$staged"

    local digest_line
    digest_line=$(sha256sum -- "$staged")
    local digest=${digest_line%% *}
    [[ "$digest" =~ ^[[:xdigit:]]{64}$ ]] || fail "sha256sum returned an invalid digest"
    printf '%s  %s\n' "$digest" "${staged#"$staging_root/"}" >"$digest_report"
    printf 'nested-kvm-stage-tests: staged %s (%s)\n' \
        "${staged#"$staging_root/"}" "$digest"
}

staged="$bin_dir/x86_cpu_snapshots"
stage_test_executable "$test_executable" "$staged" \
    "$reports_dir/nested-snapshot-tests-sha256.txt"

backend_build_json="$reports_dir/nested-kvm-backend-build.json"
[[ ! -e "$backend_build_json" && ! -L "$backend_build_json" ]] || \
    fail "build report already exists: $backend_build_json"
RUSTFLAGS='-C target-feature=+crt-static' CARGO_BUILD_JOBS=4 \
    cargo test --locked --no-run --target x86_64-unknown-linux-gnu \
    -p vmm-backend --test kvm_smoke \
    --message-format=json-render-diagnostics >"$backend_build_json"
backend_test_executable=$(select_test_executable "$backend_build_json" kvm_smoke)
validate_static_executable "$backend_test_executable" kvm_smoke
backend_staged="$bin_dir/kvm_smoke"
stage_test_executable "$backend_test_executable" "$backend_staged" \
    "$reports_dir/nested-kvm-backend-tests-sha256.txt"
