# unison implementation notes

## Deviations considered and rejected

- **`MachineError` as a tuple struct.** The spec sketches
  `pub struct MachineError(/* String or enum, via thiserror */)`; the enum
  option was taken so callers can distinguish failure classes
  (`TargetBehind`, `NoDivergence`, `DivergesAtLo`, …). The name and its use in
  every signature are unchanged.
- **A perturbation closure on `FlakyFactory`.** Rejected in favour of a
  `Perturbable: Machine` trait (implemented by `ToyMachine`) because `Machine`
  deliberately exposes no mutation hooks and closures would be harder to keep
  deterministic/serializable. `FlakyFactory<F>` keeps the exact spec'd shape;
  the `F::M: Perturbable` bound lives on its `MachineFactory` impl.
- **Arbitrary perturbations in the bisector property test.** The gate-2
  property test injects only `Perturbation::XorPrng` (with arbitrary nonzero
  masks): the xorshift64* state update is a bijection, so two states that
  differ once differ at every later step, making the injected divergence
  persistent and the ground truth exact. A register XOR can be erased by a
  later write to that register before any checkpoint observes it, which would
  make a property test flaky *by construction*, not by harness bug. Register
  perturbation is instead covered by a directed test whose program provably
  never writes the perturbed register; `ForceHalt` is covered by the gate-4
  tests.

## Semantics worth knowing (documented in code, summarized here)

- `run_to` returns `Halted` whenever the machine is halted on return —
  including a halt exactly at `target`; `ReachedTarget` strictly means "at
  target and able to continue". `target < work()` errors even on a halted
  machine.
- `compare_runs`: `limit_reached` is true only on the no-verdict-at-limit
  path; `halted_at` is `Some(w)` only when both machines halted at the same
  `w` (a `HaltMismatch` carries its own counts). A halt mismatch is
  established as soon as one machine halts and the other is observed past
  that point (`None` = "had not halted"); halt counts are compared before
  state hashes, so a forced halt reports `HaltMismatch`, not `Diverged`.
  Divergence exactly at `limit` is still observed (the last checkpoint is
  clamped to `limit`); `limit == 0` compares nothing and reports `Identical`
  with `limit_reached: true`.
- `bisect_divergence` verifies both bracket endpoints before searching
  (`NoDivergence` at `hi` — the gate-5 documented error — and `DivergesAtLo`
  for a bad `lo > 0`; `lo == 0` is trusted as the start of time). Endpoint
  verification plus `ceil(log2(hi-lo))` probes × 2 machines per probe lands
  exactly on the efficiency budget `2 * (ceil(log2(hi-lo)) + 2)`.
- Toy machine: running the pc off the end of the program halts *without*
  retiring an instruction; `JNZ` to an out-of-range pc therefore halts one
  step later. Register indices are taken mod 8 and addresses mod 65 528 at
  execution time, so any `Instr` is safe to run. The state-hash layout is
  documented at `ToyMachine::state_hash`.
- `FlakyMachine` edge cases: `diverge_at == 0` perturbs at spawn; a machine
  halting *strictly before* the boundary is never perturbed, halting *exactly
  at* it is; the perturbation is applied once and results are independent of
  how `run_to` calls are sliced. `diverge_at == u64::MAX` is the "never"
  sentinel and bails unconditionally (both in `run_to` and at spawn), so even
  a lawful `Machine` whose work counter genuinely reaches `u64::MAX` is never
  perturbed — regression-tested with a mock machine whose `run_to` jumps work
  straight to the target (PR #4 review finding).
- `spawn(0)` and `spawn(ZERO_SEED_STATE)` produce identical machines (the
  documented zero-seed mapping; xorshift64* has no zero state).

## Known limitations

- The bisector's "smallest work count in (lo, hi]" claim assumes divergence
  is persistent within the bracket (true for any state difference later
  execution cannot erase, e.g. the real-VM cases this is built for). If state
  re-converges between probes, binary search returns *a* boundary, not
  necessarily the first.
- `state_hash` re-hashes the full 64 KiB memory each call (~30 µs); fine for
  the gates, and incremental hashing is out of scope (non-goal: performance).
- The CLI is toy-only by design; `--program-seed`/`--min-work` (defaults 0 /
  10 000) select the generated demo program. `toy-bisect` brackets internally
  with `checkpoint_every = max(limit/16, 1)`. Exit codes: 0 identical, 2 for
  any detection (including `HaltMismatch`), 1 for errors.
- The binary and its deps (`clap`, `serde_json`) sit behind a default-on
  `cli` feature (`required-features` on the bin target), so library consumers
  can take a dependency-lean `default-features = false`. The CLI smoke test
  requires the feature enabled (default and `--all-features` both qualify).

## `Machine::observable_digest` (added for det-corpus / O3, task 17)

`Machine` gained `observable_digest() -> [u8; 32]`: a digest of only the
guest-**observable output** (the toy `out_log`; serial + event capture for the
real VMM), distinct from `state_hash`, which folds in latent state such as the
seed-derived entropy stream. The det-corpus O3 seed-sensitivity oracle must
compare this, not `state_hash` (which would diverge across seeds via the latent
PRNG even for a seed-pure payload, making O3 unsound).

It is a **defaulted** trait method (`default = state_hash()`), not a required
one, so it does not break existing `Machine` implementors (notably `vmm-core`'s
box-only `VmmMachine`). `ToyMachine` overrides it (output-log digest, distinct
domain tag); `FlakyMachine` delegates to its inner machine. The default is a
backward-compatible fallback and is documented as needing an override for sound
O3. Covered by `toy.rs` tests (`observable_digest_excludes_latent_prng_state`,
`observable_digest_tracks_rng_output`, `observable_digest_is_pure`).

## Mutation testing

`cargo mutants -p unison` (config in `.cargo/mutants.toml`, the path the tool
auto-discovers) is clean: every
mutant is either **caught** (the suite fails), caught by **timeout** (a
non-terminating loop, which has no other tell), or is the single documented
**equivalent mutant** below. The survivors the initial run found were all places
the suite asserted a verdict or an upper bound but never an *exact* count; they
are killed by `tests/mutation_kills.rs`:

- `lib.rs` `checkpoints_compared += 1` (both the `ReachedTarget` and the
  `Halted/Halted` arm) — mutated to `*= 1` / `-= 1` it stays pinned at 0; killed
  by asserting the exact checkpoint count on the limit-reached and halt paths.
- `lib.rs` `runs_executed += 1` (both probed machines) — the existing property
  test only checks `runs_executed <= bound`, which 0 satisfies; killed by
  asserting the exact probe count for a fixed `(0, 16]` bracket.
- `lib.rs` `if lo > 0` → `if lo >= 0` — the "trust `lo == 0` as the start of
  time" guard; killed by bisecting a machine that diverges at spawn
  (`diverge_at == 0`) from `lo == 0`, which must return work 1, not `DivergesAtLo`.
- `hex32::deserialize::nibble` uppercase arm `b'A'..=b'F'` and its `c - b'A' + 10`
  arithmetic — the serializer only emits lowercase, so the round-trip tests never
  fed an uppercase digit; killed by deserializing an uppercase-hex hash and
  asserting the decoded bytes.

### Equivalent mutant (cannot be killed)

- `hex32::deserialize`, `out[i] = (hi << 4) | lo` → `(hi << 4) ^ lo`. `hi` is a
  nibble (0..=15) shifted into bits 4–7 and `lo` is a nibble in bits 0–3, so the
  two operands never share a set bit: `|`, `^`, and `+` are all identical here.
  No input can distinguish the mutant, so it is genuinely equivalent and left as
  a documented survivor rather than chased with a test.

## For the integrator

- Point the harness at the real VMM by implementing `Machine` +
  `MachineFactory` (and `flaky::Perturbable` only if you want fault
  injection); `compare_runs`/`bisect_divergence` are generic over two factory
  types, so toy-vs-real oracle comparisons also work.
- JSON: `CompareReport`/`Verdict`/`DivergencePoint` serialize with serde
  (snake_case verdict tags, hashes as 64-char hex). `Identical` +
  `limit_reached: true` means "no divergence observed up to limit", not
  "identical forever".
- Gates run on macOS (dev machine): build, test (~50 s total, 256-case
  property tests), clippy `-D warnings`, fmt — all green. No `unsafe`, no
  platform-specific code; dependencies are whitelist-only (`thiserror`,
  `sha2`, `serde`/`serde_json`, `clap` for the bin, `proptest` dev-only).
