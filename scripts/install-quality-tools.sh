#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Install the external code-quality binaries used by the quality gates
# (.github/workflows/quality.yml and docs/CODE-QUALITY.md).
#
# These are *tools*, not crate dependencies — they are exempt from the
# Convention rule-5 dependency whitelist (tasks/00-CONVENTIONS.md).
#
# Idempotent: `cargo install` is a no-op when the requested version is already
# present, so re-running is a fast success. Safe to run on macOS and Linux.
set -euo pipefail

if ! command -v cargo >/dev/null 2>&1; then
    echo "cargo not found; install Rust first (see scripts/provision-host.sh)" >&2
    exit 1
fi

# crate            provides
#   cargo-nextest    fast, process-isolated test runner (gates)
#   cargo-llvm-cov   source-based coverage          (quality-b)
#   cargo-mutants    mutation testing               (quality-c)
#   cargo-deny       advisories/licenses/bans/sources (gates)
#   cargo-public-api public-API snapshots           (quality-d)
tools=(
    cargo-nextest
    cargo-llvm-cov
    cargo-mutants
    cargo-deny
    cargo-public-api
)

for tool in "${tools[@]}"; do
    echo "== installing ${tool} (no-op if already current)"
    cargo install --locked "${tool}"
done

echo "== installed versions"
cargo nextest --version
cargo llvm-cov --version
cargo mutants --version
cargo deny --version
cargo public-api --version

# Wire up the local fast-feedback git hooks (.githooks/pre-push: fmt, clippy,
# nextest, and Miri on the `unsafe` crates) via core.hooksPath. Convenience only —
# the gate of record is the self-hosted runner (.github/workflows/quality.yml).
# Skip with `git push --no-verify`. Run from inside the repo.
if git rev-parse --git-dir >/dev/null 2>&1; then
    echo "== configuring git hooks (core.hooksPath = .githooks)"
    git config core.hooksPath .githooks
fi

# The pre-push hook's Miri step needs a nightly toolchain with the miri component.
# Suggest it (don't force a multi-hundred-MB download on every tool install).
if ! cargo +nightly-2026-06-16 miri --version >/dev/null 2>&1; then
    echo "== note: for the local Miri pre-push step, install the pinned nightly + miri:"
    echo "     rustup toolchain install nightly-2026-06-16 --component miri"
fi
