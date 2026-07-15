# tasks/116 — Stable-bump clippy lint sweep (CI green restoration)

## Environment used

The self-hosted CI runner's stable toolchain and my Mac's stable did not match
(Mac: `rustc 1.94.1`; box: `rustc 1.96.1` at investigation start). Ran
`rustup toolchain install stable` on the box (`ssh hetzner`) to reproduce
exactly what the `gates` job's "Install stable toolchain" step does — this
itself pulled a newer stable again, to `rustc 1.97.0 (2d8144b78 2026-07-07)`.
All gates below ran under 1.97.0 on the box, in a scratch copy at
`/root/harmony-t116` (rsync'd from this worktree, not a git clone — I never
pushed this branch). `cargo-nextest`/`cargo-deny` live under
`/home/runner/.cargo/bin` (the runner user's provisioned tools per
`scripts/setup-ci-runner.sh`); ran with that dir prepended to `PATH` since I
was on the box as `root`.

## Lint sites found and fixed

Ran the `gates` job's steps in order (`.github/workflows/quality.yml`), each
against `-D warnings`, fixing failures as they surfaced and re-running from
the top until every step passed clean. Three new-lint sites, all pre-existing
code (no change in behavior — see individual notes):

1. **`clippy::byte_char_slices`** — `dissonance/control-proto/src/codec.rs:29`
   ```rust
   // before
   const MAGIC: u32 = u32::from_le_bytes([b'C', b'T', b'L', b'1']);
   // after
   const MAGIC: u32 = u32::from_le_bytes(*b"CTL1");
   ```
   `*b"CTL1"` is a `[u8; 4]` with the identical four bytes as the array
   literal it replaces — same value passed to `from_le_bytes`, so `MAGIC`'s
   value and the wire-format magic bytes it encodes are unchanged. This is
   the one wire-format-adjacent site in the sweep (`control-proto`); confirmed
   it's a pure literal-spelling change before touching it, per the task's
   stop-and-escalate instruction.

2. **`clippy::for_kv_map`** —
   `consonance/hypercall-proto/tests/stateful.rs:242`
   ```rust
   // before
   for (_id, svc) in registered.iter_mut() {
   // after
   for svc in registered.values_mut() {
   ```
   `registered` is a `BTreeMap<u16, SvcModel>`; `values_mut()` walks the same
   ascending-key order as `iter_mut()` with the key discarded, so iteration
   order (and thus the resulting `restore()` sequence) is unchanged.

3. **`clippy::manual_checked_ops`** (new in 1.97) — two sites in
   `dissonance/campaign-runner/src/campaign.rs` (the live computation at
   line ~433 and a test assertion mirroring the same formula at line ~1117):
   ```rust
   // before
   let branches_per_hour_x10 = if wall_secs == 0 {
       0
   } else {
       explored.saturating_mul(36_000) / wall_secs
   };
   // after
   let branches_per_hour_x10 = explored
       .saturating_mul(36_000)
       .checked_div(wall_secs)
       .unwrap_or(0);
   ```
   Same result for every input: `checked_div` returns `None` only when the
   divisor is `0`, at which point `unwrap_or(0)` yields the same `0` the old
   explicit branch did; otherwise it's the same integer division.

No `#[allow]`s were needed — all three lints pointed at a genuinely more
idiomatic spelling of the existing logic.

## Gates run (all green, box, `rustc`/`clippy` 1.97.0)

Reproduced every step of the `gates` job from `.github/workflows/quality.yml`:

- `cargo fmt --all -- --check`
- `cargo clippy --all-features --all-targets -- -D warnings`
- `cargo deny check`
- `cargo deny --manifest-path guest/payloads/Cargo.toml check --config deny.toml licenses`
- `cargo deny --manifest-path dissonance/control-proto/fuzz/Cargo.toml check --config deny.toml licenses`
- `cargo deny --manifest-path dissonance/flow/fuzz/Cargo.toml check --config deny.toml licenses`
- `cargo nextest run --all-features` — 1791 passed, 96 skipped, 0 failed
- `CARGO_FEATURE_NO_NEON=1 cargo clippy --target aarch64-unknown-linux-gnu --all-features --all-targets -- -D warnings`
- guest/sdk: `fmt --check`, `clippy -D warnings`, `test --all-features`,
  `deny … licenses`, `build --lib --target x86_64-unknown-none`
- `guest/payloads`: `cargo build -p sdk-demo`
- guest/flow-agent: `fmt --check`, `clippy -D warnings`,
  `test --all-features`, `deny … licenses`,
  `build --bin flow-agent --target x86_64-unknown-linux-musl`
- guest/play-agent: `fmt --check`, `clippy -D warnings`,
  `test --all-features`, `deny … licenses`, `build --bin play-agent`

All green.

## Known limitation / observation (not fixed, out of scope)

`clippy.toml`'s `disallowed-methods` entry for `rand::random` now prints
`warning: 'rand::random' does not refer to a reachable function` on every
crate under the new clippy (1.97.0) — a config-resolution diagnostic, not a
code lint; it does not fail `-D warnings` (confirmed: every `-D warnings`
invocation above still exits 0 with this warning present). It first appears
under the newly-installed stable, so it's plausibly another lint-config
compatibility gap from the toolchain bump, but chasing it is a policy/config
question (how `disallowed-methods` path resolution should be spelled for
this clippy version) rather than a mechanical lint-site fix, so it's left for
whoever owns `clippy.toml` / the toolchain-pinning follow-up noted in the
task's non-goals.

## Deviations considered and rejected

None — all three sites took the lint's own suggested rewrite verbatim (or,
for the `campaign-runner` test assertion, the same rewrite applied to keep
the assertion mirroring the code it's checking).
