# IMPLEMENTATION — task quality-d (frozen-API snapshots, cargo-public-api)

Cross-cutting infra task (hard rule 1 waived for CI, `docs/`, and adding
`tests/public_api.rs` + `tests/public-api.txt` to each crate). **No existing public
API changed** — the snapshots capture exactly what exists today.

> Originally filed as `docs/IMPLEMENTATION-quality-d.md` rather than overwriting the root
> `IMPLEMENTATION.md`, which recorded quality-a's cross-cutting work and shouldn't be
> clobbered. Task 62 relocated all three per-task diaries (this one, quality-a's, and
> task 06's) out of the authoritative-looking root/`docs/` slots into `docs/history/`,
> since none of them is *the* project's implementation notes — each is a private diary.

## What landed
- **Per-crate snapshot guard** in all four crates (`hypercall-proto`, `snapshot-store`,
  `unison`, `vtime`):
  - `tests/public-api.txt` — committed snapshot of the public surface.
  - `tests/public_api.rs` — a test that regenerates the surface with
    `cargo public-api` on the pinned nightly and asserts a byte-match, failing on drift.
- **`.github/workflows/quality.yml`** — the `public-api` job is now **gating**
  (`continue-on-error` dropped). It installs the pinned nightly + `cargo-public-api`
  and runs the four `public_api` tests on every PR.
- **`docs/CODE-QUALITY.md`** — new "Public-API snapshots" section documenting the pinned
  nightly, the generation flags, and the refresh workflow; Tier-3 entry marked adopted.

## Key decisions
- **Pinned nightly `nightly-2026-06-16`.** `cargo public-api` needs rustdoc-JSON, which is
  nightly-only. The pin lives in three places kept in sync: each `tests/public_api.rs`
  (`PINNED_NIGHTLY`), the workflow `PINNED_NIGHTLY` env, and `docs/CODE-QUALITY.md`. The
  root `rust-toolchain.toml` is unchanged (it stays stable, a root file under rule 1).
- **Generation flags `-sss`** (omit blanket, auto-trait, and auto-derived impls). These
  auto-generated impls are noisy and vary with the toolchain version; omitting them makes
  the snapshot the genuine hand-written contract and keeps it stable across nightly bumps.
- **Default features only.** Snapshots the host-side surface (the default build) that
  `vmm-core` integrates against — not a `--all-features` union (which for `hypercall-proto`
  would merge the mutually-distinct `host`/`guest` builds).
- **No new crate dependencies.** The guard shells out to the installed `cargo-public-api`
  *binary* (Convention rule-5 tool exemption) instead of adding the `public-api` /
  `rustdoc-json` library crates, keeping the dependency whitelist intact.
- **Skip-loudly when tooling absent.** If the pinned nightly or `cargo-public-api` is not
  installed, the test prints `SKIP: …` and returns rather than failing, so a plain
  stable-only `cargo nextest` stays green for devs without the nightly tooling. CI installs
  both, so the gate runs for real there. A genuine build error (not a missing-tool error)
  still fails the test.
- **No `--locked` in CI.** This workspace does not commit a `Cargo.lock` (it is neither
  tracked nor gitignored), matching the other jobs which also omit `--locked`.

## Verification (acceptance gate 2)
Temporarily appended `pub fn __drift_probe() {}` to `consonance/vtime/src/lib.rs`. The
`vtime` `public_api` test **failed** with the drift shown as the added line
`pub fn vtime::__drift_probe()` in the actual surface vs. the committed snapshot. Reverted
the change (`git checkout`); the test passes again. Confirmed the guard has teeth.

## Gates run (all green)
- `cargo fmt --all -- --check`
- `cargo clippy -p <crate> --all-features --all-targets -- -D warnings` (four crates; the
  only output is pre-existing `clippy.toml` config warnings about `rand::*` paths, present
  on `main` and unrelated to this task)
- `cargo nextest run -p hypercall-proto -p snapshot-store -p unison -p vtime --all-features`
  (127 passed, 2 skipped) — includes the four new `public_api` tests passing for real
- `cargo test … --test public_api` (the exact CI command) — four pass
- `cargo deny check` — advisories/bans/licenses/sources ok

## For the integrator
- Bumping the nightly: update `PINNED_NIGHTLY` in all four `tests/public_api.rs`, the
  workflow env, and `docs/CODE-QUALITY.md`, then regenerate snapshots with
  `UPDATE_PUBLIC_API=1 cargo test -p <crate> --test public_api` and review the diffs.
- An intentional, reviewed public-API change must be accompanied by a refreshed
  `tests/public-api.txt` (same `UPDATE_PUBLIC_API=1` command); otherwise the gate fails.
