#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the one full-VMM test binary that the nested boot harness stages into
# its disposable initramfs. This script never runs the test.
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

test_executable=$(python3 - "$build_json" <<'PY'
import json
import sys

path = sys.argv[1]
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
        if not isinstance(target, dict) or target.get("name") != "x86_cpu_snapshots":
            continue
        kinds = target.get("kind")
        if not isinstance(kinds, list) or "test" not in kinds:
            continue
        executable = message.get("executable")
        if not isinstance(executable, str) or not executable:
            raise SystemExit("matching x86_cpu_snapshots artifact has no executable")
        matches.append(executable)

unique = list(dict.fromkeys(matches))
if len(unique) != 1:
    if not unique:
        raise SystemExit("cargo produced no x86_cpu_snapshots test executable")
    raise SystemExit(
        "cargo produced multiple x86_cpu_snapshots test executables: "
        + ", ".join(unique)
    )
print(unique[0])
PY
)

[[ -f "$test_executable" ]] || fail "selected test executable is not a regular file: $test_executable"
[[ -x "$test_executable" ]] || fail "selected test executable is not executable: $test_executable"
program_headers=$(readelf -l -- "$test_executable") || \
    fail "cannot inspect selected test executable: $test_executable"
if printf '%s\n' "$program_headers" | grep -Eq '^[[:space:]]*INTERP([[:space:]]|$)'; then
    fail "selected test executable is dynamically linked: $test_executable"
fi

bin_dir="$staging_root/bin"
if [[ -L "$bin_dir" || (-e "$bin_dir" && ! -d "$bin_dir") ]]; then
    fail "staging bin path is not a directory: $bin_dir"
fi
mkdir -p -- "$bin_dir"
staged="$bin_dir/x86_cpu_snapshots"
[[ ! -e "$staged" && ! -L "$staged" ]] || fail "staging destination already exists: $staged"
cp -- "$test_executable" "$staged"
chmod 0555 -- "$staged"

digest_line=$(sha256sum -- "$staged")
digest=${digest_line%% *}
[[ "$digest" =~ ^[[:xdigit:]]{64}$ ]] || fail "sha256sum returned an invalid digest"
printf '%s  bin/x86_cpu_snapshots\n' "$digest" >"$reports_dir/nested-snapshot-tests-sha256.txt"
printf 'nested-kvm-stage-tests: staged static x86_cpu_snapshots (%s)\n' "$digest"
