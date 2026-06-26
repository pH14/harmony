# IMPLEMENTATION — task quality-a (determinism lints, cargo-deny, CI scaffold)

This is PR-A of the code-quality plan (`docs/CODE-QUALITY.md`): the scaffold that
mechanizes Convention rule #4 and stands up the CI that quality-b…f plug into.
It is a cross-cutting infra task (hard rule 1 waived for the listed files); no
crate logic or public API changed.

## What landed
- **`clippy.toml`** — verbatim determinism lints from the spec: disallowed methods
  (`Instant::now`, `SystemTime::now`, `rand::thread_rng`, `rand::random`) and
  disallowed types (`HashMap`, `HashSet`).
- **`deny.toml`** — advisories (RustSec, yanked=deny), bans, licenses (MIT /
  Apache-2.0 [+LLVM-exception] / Unicode-3.0 / BSD-2/3-Clause, deny rest),
  sources (crates.io only).
- **`.github/workflows/quality.yml`** — gating `gates` job (matrix
  ubuntu+macos: fmt, clippy `-D warnings`, `cargo deny check`, `cargo nextest`)
  plus `coverage`/`mutants`/`public-api` placeholders (`continue-on-error: true`),
  named exactly so later PRs append rather than rewrite.
- **`scripts/install-quality-tools.sh`** — idempotent `cargo install --locked`
  for nextest/llvm-cov/mutants/deny/public-api; prints versions; shellcheck-clean.
- **`tasks/00-CONVENTIONS.md`** — gates block now lists `cargo nextest`
  (subsumes `cargo test`) + `cargo deny check` and notes clippy enforces
  `clippy.toml`; rule 5 adds `proptest-state-machine`/`arbitrary` dev-deps and
  exempts the external quality binaries.
- **Annotated exceptions** (no logic change): `snapshot-store`'s lookup-only
  `resolve_cache` HashMap (3 sites) and the informational `tests/bench.rs`
  `Instant` timing, each `#[allow(...)]` + `// not order-observable:` justified.

## Violations review (deliverable 2)
The new lints surfaced exactly two legitimate, order-unobservable uses, both in
`snapshot-store`, both annotated. **No use where iteration order could reach an
output/hash/byte was found** — nothing was silenced that should have been fixed.

## Decisions considered & rejected
- **`bans.multiple-versions = "deny"`** → set to **`"warn"`**. The only duplicates
  are dev-only *transitive* deps (getrandom/cpufeatures/wit-bindgen pulled at
  different versions by proptest vs tempfile); they're unfixable without forking
  upstream and the exact set shifts with platform/feature resolution, so gating CI
  on them is the un-reasonable case of the spec's "where reasonable". `wildcards`
  stays `deny`. (Hard-failing here would make CI red on transitive churn.)
- **Committing `Cargo.lock`** → not committed. The workspace does not track a
  lockfile and gate 5 scopes the allowed file set; adding it would exceed scope.

## Known limitations / integrator notes
- The `coverage`/`mutants`/`public-api` jobs are intentional placeholders — they
  run the tool only if present and never gate. quality-b/c/d harden them; keep the
  job names.
- The `rand::*` clippy paths emit a one-time "does not refer to a reachable
  function" note (rand is only a dev-dep via proptest). It is informational, does
  not fail `-D warnings`, and is correct to keep per the spec's verbatim config.
- `cargo deny check` needs network on first run to fetch the advisory DB.

## Gates verified locally (macOS)
`cargo clippy --all-features --all-targets -- -D warnings` ✓ · `cargo deny check`
✓ (advisories/bans/licenses/sources ok) · `cargo fmt --all -- --check` ✓ ·
`cargo nextest run --all-features` ✓ (123 passed) · `actionlint quality.yml` ✓ ·
`shellcheck install-quality-tools.sh` ✓.
