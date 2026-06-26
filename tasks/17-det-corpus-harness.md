# Task 17 — `consonance/det-corpus`: oracle runner & corpus manifest

Read `tasks/00-CONVENTIONS.md` first, then `docs/DETERMINISM-CORPUS.md` (the design this
implements). Touch `consonance/det-corpus/` **plus** the small additive `unison` accessor that O3
requires (see the `check_seed_sensitivity` note): add `Machine::observable_digest() -> [u8; 32]`
(guest-emitted serial + event-log bytes, distinct from `state_hash`) to `consonance/unison/` and
update its `tests/public-api.txt`. That two-crate scope is deliberate and the **only** permitted
edit outside `consonance/det-corpus/` — O3 is unimplementable without it (current `Machine` exposes
only `state_hash()`). Confirm the accessor shape with the integrator if it diverges from this.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does **not** require: `/dev/kvm`, Intel CPU,
QEMU, root. (The real-VMM adapter is integration-class and lands later; everything in this
task is tested against the `unison` `ToyMachine`.)

## Context

`unison` is a generic, domain-free divergence bisector over a `Machine` trait
(`compare_runs` / `bisect_divergence`). This crate is the **domain layer**: it turns that
primitive into the three determinism oracles (O1–O3, `docs/DETERMINISM-CORPUS.md`) and a corpus
manifest that says which oracles apply to which workload. It is written **generically over
`unison::MachineFactory`** so it is fully testable now with `ToyFactory` / `FlakyFactory`,
and pointed at `vmm-core::Vmm<B>` at integration with no API change.

This crate **may** depend on `unison` (path dep) — it is the integration layer that binds
the bisector to domain knowledge, not a wave-1 parallel crate, so rule 2 ("no sibling deps")
does not apply. Beyond `unison` it needs `serde`+`serde_json` (whitelisted) for the JSON
report. The manifest is TOML to match `cpu-msr-contract.toml`; parsing it needs the `toml`
crate, which is **outside** the conventions whitelist — request it ask-by-comment in the PR
(the repo already ships `.toml` artifacts), or, if the integrator prefers no new dep, make the
manifest `corpus-manifest.json` and parse with the whitelisted `serde_json`. Default: request
`toml`.

## Public API

std crate: a library plus a `det-corpus` binary.

```rust
/// Which oracle(s) a corpus item participates in. See docs/DETERMINISM-CORPUS.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Oracle {
    /// O1: two runs at the same seed are bit-identical (localized on failure).
    Determinism,
    /// O2: observed state digest equals a committed golden.
    Conformance,
    /// O3: behaviour under two *different* seeds is non-trivial in the declared way.
    SeedSensitivity { rng_consuming: bool },
}

/// One registered workload. Parsed from `corpus-manifest.toml`; also constructible directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusItem {
    pub name: String,
    pub kind: CorpusKind,          // Micro | Workload | FuzzSeed
    pub source: String,            // path to payload / generator input, relative to repo root
    pub oracles: Vec<Oracle>,
    pub golden: Option<String>,    // path to golden digest, required iff Conformance ∈ oracles
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusKind { Micro, Workload, FuzzSeed }

/// Parse / serialize the manifest. Deterministic field order (no HashMap into output).
pub fn load_manifest(toml: &str) -> Result<Vec<CorpusItem>, ManifestError>;
pub fn to_manifest(items: &[CorpusItem]) -> String;

/// Per-item outcome. `Pass`/`Fail` per oracle; the report is the aggregate.
pub struct ItemReport {
    pub name: String,
    pub results: Vec<OracleResult>,
}
pub struct OracleResult {
    pub oracle: Oracle,
    pub passed: bool,
    /// Set on O1 failure: the exact divergence point from the bisector.
    pub divergence: Option<unison::DivergencePoint>,
    /// Human-readable detail (golden mismatch summary, seed-sensitivity violation, …).
    pub detail: String,
}

/// O1 — run the same factory twice at `seed`; on divergence, bisect and attach the point.
pub fn check_determinism<F: unison::MachineFactory>(
    f: &F, seed: u64, checkpoint_every: u64, limit: u64,
) -> Result<OracleResult, unison::MachineError>;

/// O2 — run once at `seed`, compare the final `state_hash` (hex) to `golden_hex`.
pub fn check_conformance<F: unison::MachineFactory>(
    f: &F, seed: u64, limit: u64, golden_hex: &str,
) -> Result<OracleResult, unison::MachineError>;

/// O3 — run at `seed_a` and `seed_b` (must differ). Compares a guest-**observable
/// output** digest (`out_*`), **NOT** `state_hash`. Why: the real VMM seeds its entropy
/// device from the run seed, so `state_hash` — which includes that latent PRNG state
/// (INTEGRATION.md §4) — differs across seeds even for a payload that never consumes RNG,
/// and a *broken* constant-`RDRAND` payload could still show `hash_a != hash_b` for that
/// same latent reason. So the oracle is unsound on `state_hash`. Using the observable
/// digest: if `rng_consuming`, assert `work_a == work_b` (control flow seed-stable) AND
/// `out_a != out_b` (the seed actually reached observable output); else assert
/// `out_a == out_b` (nothing seed-dependent reached output).
///
/// `out_*` is `Machine::observable_digest()` — the guest-emitted serial + event-log bytes,
/// distinct from `state_hash`. This task **adds** that accessor to `unison` (see the
/// scope note at the top): the current trait exposes only `state_hash()`, which conflates
/// observable output with latent device state (the seeded entropy stream), so O3 is
/// unsound on it. `ToyMachine` implements `observable_digest` as its emitted output; the
/// real-VMM adapter (later) implements it from the serial + event capture.
pub fn check_seed_sensitivity<F: unison::MachineFactory>(
    f: &F, seed_a: u64, seed_b: u64, limit: u64, rng_consuming: bool,
) -> Result<OracleResult, unison::MachineError>;

pub struct ManifestError(/* via thiserror */);
```

The binary: `det-corpus run --manifest <path> [--item NAME] [--seed S]` runs the applicable
oracles for each item (delegating to a caller-supplied factory registry — for this task, the
toy registry; the VMM registry is wired at integration) and prints one JSON object
(`serde_json`) with the `ItemReport`s. Exit 0 if all pass, 2 if any oracle fails (it is a
detector). A `det-corpus validate --manifest <path>` subcommand round-trips the manifest and
checks every `Conformance` item has a `golden`.

## Acceptance gates

Beyond the standard gates (`tasks/00-CONVENTIONS.md`):

1. **O1 pass/fail** (property test): for an arbitrary toy program + seed, `check_determinism`
   on `ToyFactory` returns `passed: true`. The negative case must be **non-vacuous**:
   `check_determinism` compares two machines spawned from the *same* factory, so a `FlakyFactory`
   that applies the *same* perturbation at the same work count to both spawns stays identical
   (vacuous). The factory must perturb **differently across spawns** — e.g. `FlakyFactory`
   carries a spawn counter and only the **second** spawn diverges at `T` (or exposes a
   clean-vs-flaky pair) — so `check_determinism` returns `passed: false` with
   `divergence.first_divergent_work == T`. State the chosen mechanism. ≥256 cases.
2. **O2 pass/fail**: `check_conformance` against the digest of an actual toy run passes; against
   any other hex fails with a mismatch detail; a malformed/short hex is a `Fail`, never a panic.
3. **O3 both directions** (the anti-cheat gate): build two toy programs — one that calls `RAND`
   then `OUT` without branching on it (control-flow-stable, RNG-consuming), one with no `RAND`.
   For the RNG program, `check_seed_sensitivity(rng_consuming: true)` passes (equal `work`,
   differing `hash`) and **fails** if pointed at a `ToyFactory` variant whose `RAND` is stubbed
   to a constant (the faked-determinism case). For the pure program,
   `check_seed_sensitivity(rng_consuming: false)` passes, and **fails** if pointed at a factory
   that leaks the seed into state. ≥256 cases each direction.
4. **Manifest round-trip** (property test): `load_manifest(to_manifest(items)) == items` for
   arbitrary item lists; field order in output is deterministic; `validate` rejects a
   `Conformance` item with no `golden`.
5. **Report JSON**: a CLI integration test (`std::process::Command`) runs a 2-item toy manifest,
   parses the JSON, asserts exit code and per-oracle results; one failing item flips exit to 2.
6. **No-panic on bad input**: `load_manifest` on garbage TOML returns `Err`, never panics
   (library-code-never-panics rule).

## Non-goals

Any KVM/VMM integration or the `Vmm` factory (frontier — this is generic over `MachineFactory`);
O4 backend-equivalence (needs two real backends; trivially expressible later as
`compare_runs(F_a, F_b, …)` and noted in `IMPLEMENTATION.md`); building payloads or goldens
(tasks 18/20); the fuzzer (task 19); pretty output (JSON only).
