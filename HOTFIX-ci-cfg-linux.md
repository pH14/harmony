# Hotfix: main CI red — cfg(linux) compile + clippy errors

The `quality` CI (Linux self-hosted runners) is red on main. These errors only
manifest under `#[cfg(target_os = "linux")]`, which macOS-local `cargo` skips —
so they slipped past Mac-local review gates.

Fix (all cfg-linux):
1. `dissonance/conductor/src/main.rs:682` and `dissonance/conductor/tests/live_materialization.rs:128`
   — `match vmm.step()` is non-exhaustive: `Step::SdkStop` (added by task 73 PR B #63)
   is not covered → `E0004`. Add an `Ok(Step::SdkStop) => ...` arm (map to a step error /
   the appropriate terminal, matching intent).
2. `consonance/vmm-core/tests/live_sdk.rs` — clippy `-D warnings`: "very complex type used" —
   factor a `type` alias.
3. `dissonance/conductor` (lib) — clippy: "manual checked division" — use `checked_div` (or the idiomatic form).

Verify on the box (ssh): the cfg(linux) build + clippy must be clean, since Mac can't compile these paths.
