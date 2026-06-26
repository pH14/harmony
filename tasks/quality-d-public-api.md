# Task quality-d — frozen-API snapshots (cargo-public-api)

Read `tasks/00-CONVENTIONS.md` first. **Cross-cutting infra task; rule 1 waived** for CI and
adding `tests/public_api.rs` + `tests/public-api.txt` to each crate. You must NOT change any
existing public API.

## Dependency
**Requires `quality-a` merged.** Branch from updated `main`. Needs the `public-api`
placeholder job; stop and report if absent.

## Environment
Runs on: macOS and Linux (snapshot check). Requires: Rust + `cargo-public-api` (needs a
rustdoc-JSON-capable nightly — pin and document it). No `/dev/kvm`.

## Context
The delegated crates' public APIs are frozen contracts (Convention rule 3, INTEGRATION.md
"frozen" seams) that `vmm-core` integrates against later. Make drift a reviewable diff. See
`docs/CODE-QUALITY.md`.

## Deliverables
1. For each of the four crates (`hypercall-proto`, `snapshot-store`, `unison`, `vtime`)
   add a committed public-surface snapshot `tests/public-api.txt` and a test
   (`tests/public_api.rs`) that regenerates the surface and asserts it matches, failing on
   drift. Pin the exact nightly toolchain the tool needs and document it in
   `docs/CODE-QUALITY.md`.
2. Harden the CI `public-api` job (drop `continue-on-error`): regenerate + diff on every PR.

## Acceptance gates
1. Snapshots committed for all four crates; the `public_api` test passes for each.
2. Verification: temporarily add a dummy `pub fn` to one crate, confirm the test FAILS, then
   revert — note in your IMPLEMENTATION.md that you verified this.
3. The `public-api` CI job is gating.
4. `git diff` adds only `tests/public-api.txt` + `tests/public_api.rs` per crate, `.github/`,
   and `docs/` — no change to existing public items.

## Non-goals
Changing or "cleaning up" any public API. Snapshot exactly what exists today.
