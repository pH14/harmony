# Task quality-b — coverage gate (cargo-llvm-cov)

Read `tasks/00-CONVENTIONS.md` first. **Cross-cutting infra task; rule 1 waived** for CI,
scripts, and `docs/`. No crate logic changes.

## Dependency
**Requires `quality-a` merged.** Branch from updated `main` (you need its `quality.yml`
scaffold and conventions). If the `coverage` placeholder job is absent, stop and report.

## Environment
Runs on: macOS and Linux. Requires: Rust + `cargo-llvm-cov` + `cargo-nextest`. No `/dev/kvm`.

## Context
Coverage measures reachability (its partner, mutation, is quality-c). Goal here: a gating,
ratcheting coverage floor measured from reality — never a round number. See
`docs/CODE-QUALITY.md`.

## Deliverables
1. Harden the `coverage` job in `.github/workflows/quality.yml` (drop `continue-on-error`):
   ubuntu, stable + `llvm-tools-preview`; run
   `cargo llvm-cov nextest --all-features --lcov --output-path lcov.info` and enforce a
   **region** floor via `--fail-under-regions <FLOOR>`.
2. Measure the current workspace region coverage, set `FLOOR` to `floor(measured)` (no
   margin padding beyond rounding down), and record the per-crate + workspace numbers in a
   new `## Coverage baseline (<today's date>)` table in `docs/CODE-QUALITY.md`.
3. `scripts/coverage.sh`: runs `cargo llvm-cov nextest` writing both `lcov.info` and an HTML
   report; shellcheck-clean; works on macOS + Linux.

## Acceptance gates
1. The `coverage` CI job passes at the committed `FLOOR`.
2. The baseline table is committed with real measured numbers (not invented).
3. `scripts/coverage.sh` runs locally on this machine and produces a report.
4. `git diff` touches only `.github/`, `scripts/`, `docs/` — no crate changes.

## Non-goals
Writing new tests to raise coverage (organic, later). Only the gate + measured baseline.
