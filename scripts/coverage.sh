#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Measure workspace test coverage with cargo-llvm-cov + cargo-nextest.
#
# Produces two artifacts under target/llvm-cov/:
#   - lcov.info            machine-readable LCOV (for CI / badges)
#   - html/index.html      human-readable HTML report
#
# Region coverage is the project's chosen metric (see docs/CODE-QUALITY.md);
# this script only *reports* — the gating floor lives in
# .github/workflows/quality.yml. Runs on macOS and Linux; no /dev/kvm needed.
set -euo pipefail

if ! cargo llvm-cov --version >/dev/null 2>&1; then
    echo "cargo-llvm-cov not found; run scripts/install-quality-tools.sh" >&2
    exit 1
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

out_dir="target/llvm-cov"
lcov_path="${out_dir}/lcov.info"
mkdir -p "${out_dir}"

# Single instrumented run, emitting LCOV; --no-report keeps the profile data so
# the follow-up --report invocations reuse it instead of re-running the tests.
cargo llvm-cov nextest --all-features --no-report

cargo llvm-cov report --lcov --output-path "${lcov_path}"
cargo llvm-cov report --html --output-dir "${out_dir}"

echo "== coverage artifacts"
echo "  lcov: ${lcov_path}"
echo "  html: ${out_dir}/html/index.html"
