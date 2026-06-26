# Task quality-a — determinism lints, cargo-deny, CI scaffold

Read `tasks/00-CONVENTIONS.md` first. **This is a cross-cutting infrastructure task: hard
rule 1 ("touch only your directory") is WAIVED for the files listed below.** You may edit
workspace-root config, CI, scripts, and conventions. You must NOT change any crate's logic
or public API — only add annotated `#[allow(...)]` where a new lint flags a legitimate use.

## Environment
Runs on: macOS and Linux. Requires: Rust, plus `cargo-nextest` and `cargo-deny`
(`scripts/install-quality-tools.sh`, which you write). Does not require `/dev/kvm`.

## Context
The project's prime invariant (Convention rule #4: no `HashMap`/`HashSet` iteration into
outputs, no float in state, no wall-clock, no unseeded RNG) is enforced by review only.
This task mechanizes it and stands up the CI that all later quality tasks (quality-b…f)
plug into. See `docs/CODE-QUALITY.md` for the full plan; this is the scaffold (PR-A).

## Deliverables
1. **`clippy.toml`** (repo root) with:
   ```toml
   disallowed-methods = [
     { path = "std::time::Instant::now",    reason = "wall-clock leaks nondeterminism; use vtime" },
     { path = "std::time::SystemTime::now", reason = "wall-clock leaks nondeterminism" },
     { path = "rand::thread_rng",           reason = "unseeded RNG; thread a seed explicitly" },
     { path = "rand::random",               reason = "unseeded RNG" },
   ]
   disallowed-types = [
     { path = "std::collections::HashMap",  reason = "iteration order can reach a hash/byte; use BTreeMap or sort" },
     { path = "std::collections::HashSet",  reason = "iteration order can reach a hash/byte; use BTreeSet or sort" },
   ]
   ```
2. **Resolve the violations the new lints surface** across the four crates. For each
   legitimate lookup-only use (e.g. `snapshot-store`'s per-layer memo index, where the map
   never reaches an output), add `#[allow(clippy::disallowed_types)]` with a
   `// not order-observable: <reason>` comment. **If you find a use where order COULD reach
   an output/hash/byte, STOP and report it — do not silence it.** Do not change logic.
3. **`deny.toml`** (repo root): `advisories` (RustSec, deny vulnerabilities/unmaintained),
   `bans` (deny multiple-versions where reasonable), `licenses` (allow MIT, Apache-2.0,
   Unicode-3.0, BSD; deny the rest), and `sources` (crates.io only). Allowlist mirrors the
   Convention rule-5 dependency set.
4. **`.github/workflows/quality.yml`**:
   - job **`gates`** (matrix `os: [ubuntu-latest, macos-latest]`, stable toolchain): install
     `cargo-nextest` + `cargo-deny` via `taiki-e/install-action`; run `cargo fmt --all --
     --check`, `cargo clippy --all-features --all-targets -- -D warnings`,
     `cargo deny check`, `cargo nextest run --all-features`. **Gating.**
   - jobs **`coverage`**, **`mutants`**, **`public-api`** (ubuntu): minimal placeholders with
     `continue-on-error: true`, each invoking its tool if present. quality-b/c/d will harden
     these. Keep them named exactly so later PRs append, not rewrite.
5. **`scripts/install-quality-tools.sh`**: idempotent (`cargo install --locked …` for
   nextest, llvm-cov, mutants, deny, public-api), prints versions, shellcheck-clean. Mirror
   the style of the existing provisioning script.
6. **`tasks/00-CONVENTIONS.md`** edits: in the Gates block add `cargo nextest run
   --all-features` (note it subsumes `cargo test`) and `cargo deny check`, and note clippy now
   enforces `clippy.toml`. Add to rule 5: the external quality *binaries*
   (nextest/llvm-cov/mutants/deny/public-api) are installed tools, NOT dependencies, so they
   are exempt from the whitelist; and ADD `proptest-state-machine` and `arbitrary` to the
   dev-dependency whitelist (quality-e and future fuzzing use them).

## Acceptance gates
1. `cargo clippy --all-features --all-targets -- -D warnings` passes workspace-wide with the
   new `clippy.toml` (legit exceptions annotated; no logic changes).
2. `cargo deny check` passes (advisories, bans, licenses, sources).
3. `.github/workflows/quality.yml` parses (YAML valid; `actionlint` clean if available).
4. `scripts/install-quality-tools.sh` is shellcheck-clean and idempotent (running twice is a
   no-op success).
5. `git diff` shows changes ONLY in: `clippy.toml`, `deny.toml`, `.github/`, `scripts/`,
   `tasks/00-CONVENTIONS.md`, and annotated `#[allow]`/comments inside crates — no crate
   logic or public-API changes.

## Non-goals
Coverage thresholds (quality-b), mutation triage (quality-c), public-api snapshots
(quality-d), proptests (quality-e), Kani (quality-f). Don't pre-empt them.
