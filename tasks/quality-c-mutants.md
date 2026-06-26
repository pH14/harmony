# Task quality-c — mutation testing (cargo-mutants), unison first

Read `tasks/00-CONVENTIONS.md` first. **Cross-cutting infra task; rule 1 waived** for CI,
root config, and `docs/`. You MAY add/strengthen TESTS in `consonance/unison/`; you must NOT
change `unison` library logic or public API.

## Dependency
**Requires `quality-a` merged.** Branch from updated `main`. Needs the `mutants` placeholder
job; stop and report if absent.

## Environment
Runs on: macOS and Linux. Requires: Rust + `cargo-mutants`. No `/dev/kvm`.

## Context
Mutation testing proves the gates actually constrain behaviour. The highest-value target is
`unison` — the determinism oracle that certifies every other component; if its bisector
tests don't bite, every downstream gate is compromised. So clean it first. See
`docs/CODE-QUALITY.md`.

## Deliverables
1. **`mutants.toml`** (root): exclude the CLI `main.rs` from mutation, set a
   `timeout_multiplier`, and `additional_cargo_test_args`/test-tool config so mutants runs
   the nextest suite.
2. **Make `unison` mutation-clean.** Run `cargo mutants -p unison`. For every
   surviving mutant, EITHER add/strengthen a test in `consonance/unison/tests/` to kill it,
   OR document it as a justified equivalent mutant in a new "## Mutation testing" section of
   `consonance/unison/IMPLEMENTATION.md`. End state: zero un-triaged survivors.
3. Harden the CI `mutants` job (drop `continue-on-error`): run `cargo mutants --in-diff`
   against the PR diff so future PRs are gated on changed-line mutation.
4. Note the approach + per-crate status in `docs/CODE-QUALITY.md`.

## Acceptance gates
1. `cargo mutants -p unison` reports zero surviving mutants that are not documented as
   equivalent in `IMPLEMENTATION.md`.
2. New/changed files are limited to: `mutants.toml`, `.github/`, `docs/`, and
   `consonance/unison/tests/` + `consonance/unison/IMPLEMENTATION.md`. No library logic or
   public-API change.
3. The `mutants` CI job runs `--in-diff` and is gating.

## Non-goals
Full-tree zero-survivor on all crates — only `unison` must be clean now; the others get
the `--in-diff` gate going forward. (Follow-up tasks can harden them.)
