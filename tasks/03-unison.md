# Task 03 — `unison`: determinism test harness & divergence bisector

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/unison/`.

## Environment

Runs on: macOS and Linux. Requires: Rust only. Does not require: `/dev/kvm`, Intel CPU,
QEMU, root.

## Context

The project's central invariant is: same seed ⇒ bit-identical execution. Every phase of the
hypervisor is gated on it, and when it breaks, the only tractable debugging method is
**bisection by work count**: find the exact unit of work at which two supposedly-identical
runs first diverge. This crate is that harness, built against an abstract `Machine` trait so
it can be developed and fully tested now using a toy interpreter, and pointed at the real VMM
later by implementing the same trait. The toy machine doubles as the harness's own test
fixture: we wrap it to *inject* divergence at a known point and assert the bisector finds
exactly that point.

"Work" is an abstract monotonic counter (for the real VM it will be retired branches; for the
toy machine, instructions executed). All harness logic must treat it as opaque ticks.

## Public API

std crate: a library plus a `unison` binary.

```rust
/// A deterministic machine under test. Implementations must guarantee that two
/// instances created with the same seed behave identically — that is the property
/// this harness exists to check.
pub trait Machine {
    /// Run until work() == target or the machine halts, whichever first.
    /// target < work() is an error (machines cannot run backwards).
    fn run_to(&mut self, target: u64) -> Result<RunOutcome, MachineError>;
    fn work(&self) -> u64;
    /// Canonical hash of ALL architectural state (registers, memory, output log…).
    /// Must be a pure function of state — calling it twice changes nothing.
    fn state_hash(&self) -> [u8; 32];
}

#[derive(Debug, PartialEq)] pub enum RunOutcome { ReachedTarget, Halted }
pub struct MachineError(/* String or enum, via thiserror */);

/// Creates fresh machines. Bisection re-executes from scratch many times, so
/// spawning must be cheap and, above all, deterministic.
pub trait MachineFactory {
    type M: Machine;
    fn spawn(&self, seed: u64) -> Self::M;
}

pub struct CompareReport {
    pub verdict: Verdict,
    pub checkpoints_compared: u64,
    pub halted_at: Option<u64>,
    /// True if the comparison stopped because `limit` was reached rather than
    /// because both machines halted. An `Identical` verdict with `limit_reached`
    /// means "no divergence observed up to limit", NOT "the runs are identical
    /// forever" — callers (and the CLI JSON output) must surface the distinction.
    pub limit_reached: bool,
}
pub enum Verdict {
    Identical,
    /// Hashes matched at `last_match` (or from the start if None) and differed at
    /// `first_mismatch` — the divergence lies in (last_match, first_mismatch].
    Diverged { last_match: Option<u64>, first_mismatch: u64 },
    /// One machine halted at a different work count than the other.
    HaltMismatch { a: Option<u64>, b: Option<u64> },
}

/// Run a fresh machine from each factory with the same seed, hashing state every
/// `checkpoint_every` work units (and at halt), until both halt or `limit` is reached.
pub fn compare_runs<FA: MachineFactory, FB: MachineFactory>(
    a: &FA, b: &FB, seed: u64, checkpoint_every: u64, limit: u64,
) -> Result<CompareReport, MachineError>;

pub struct DivergencePoint {
    /// Smallest work count w in (lo, hi] where state hashes differ.
    pub first_divergent_work: u64,
    pub hash_a: [u8; 32],
    pub hash_b: [u8; 32],
    pub runs_executed: u64, // for the efficiency gate
}

/// Binary-search the exact divergence point, given a bracketing interval from
/// compare_runs: hashes match at `lo` (or lo == 0), differ at `hi`. Each probe
/// spawns fresh machines and runs to the midpoint — O(log(hi-lo)) probes total.
pub fn bisect_divergence<FA: MachineFactory, FB: MachineFactory>(
    a: &FA, b: &FB, seed: u64, lo: u64, hi: u64,
) -> Result<DivergencePoint, MachineError>;
```

### Reference machine: `toy` module

A tiny deterministic register VM used for testing the harness (and later as a sanity oracle).
Spec (normative — tests depend on it):

- State: 8 × u64 registers `r0..r7`; 65 536 bytes of memory; program counter; an append-only
  output log; an xorshift64\* PRNG state seeded from `seed` (zero seed maps to a documented
  nonzero constant); halted flag. `work` = instructions retired.
- Instructions (each costs exactly 1 work unit): `LOADI rd, imm64`; `ADD/SUB/XOR rd, rs`;
  `LOAD rd, [rs]` / `STORE [rd], rs` (u64, little-endian, address taken mod 65 528);
  `JNZ rs, target_pc`; `RAND rd` (next PRNG value); `OUT rs` (append 8 bytes to output log);
  `HALT`. Encoding is implementer's choice; provide a small assembler helper for tests.
- `state_hash` = sha2-256 over a canonical serialization of: registers, pc, full memory,
  output log, PRNG state, halted flag (document the exact layout in code).
- Provide `ToyFactory { program: Vec<Instr> }` implementing `MachineFactory` (PRNG seed comes
  from `spawn(seed)`), and a program generator for property tests: random programs that are
  guaranteed to keep running (e.g. a bounded loop skeleton with random straight-line bodies)
  for at least N instructions.

### Divergence injection: `FlakyFactory`

`FlakyFactory<F: MachineFactory> { inner: F, diverge_at: u64, perturb: Perturbation }` wraps
spawned machines so that the **first time** work reaches ≥ `diverge_at`, a perturbation is
applied once (e.g. XOR `r0` with a constant; `Perturbation` is a small enum). A
`FlakyFactory` with `diverge_at: u64::MAX` behaves identically to its inner factory. This
simulates "run B has a nondeterminism bug at tick T" with T known, so the bisector can be
tested against ground truth. Take care that the perturbation applies *at* the boundary even
when a `run_to` target lands beyond it (run to the boundary internally, perturb, continue).

### CLI

`unison toy-compare --seed S --diverge-at T --checkpoint-every C --limit L` and
`unison toy-bisect --seed S --diverge-at T --limit L`: run the toy machine vs. its flaky
wrapper, print a single JSON object (serde_json) with the report/divergence point. Exit code
0 if Identical, 2 if Diverged (it's a detector, not a failure). This CLI is a demo/debug tool
for the toy machine only; the real-VM adapter arrives later.

## Acceptance gates

Beyond the standard gates:

1. **Toy determinism property test**: arbitrary generated program + seed: two fresh spawns
   run to the same target have equal hashes at every checkpoint and equal final state.
2. **Bisector exactness property test** (the core gate): arbitrary (program, seed,
   `diverge_at` within run length, perturbation): `compare_runs(toy, flaky, ...)` brackets the
   divergence, then `bisect_divergence` returns `first_divergent_work == diverge_at`,
   for ≥ 256 cases including `diverge_at` ∈ {1, limit-1, exact checkpoint boundaries,
   checkpoint boundary ± 1}.
3. **Efficiency gate**: in the bisector property test, assert
   `runs_executed ≤ 2 * (ceil(log2(hi - lo)) + 2)`.
4. **HaltMismatch test**: flaky perturbation that forces an early HALT yields
   `Verdict::HaltMismatch` with correct counts.
5. **No-divergence path**: `FlakyFactory` with `diverge_at = u64::MAX` ⇒ `Identical`, and
   `bisect_divergence` on a non-divergent pair returns a documented error (not a bogus point).
6. **CLI smoke test**: invoke both subcommands via `std::process::Command` in an integration
   test; parse the JSON; check the divergence point round-trips.

## Non-goals

Any KVM/VMM integration; trace recording or instruction-level logging; parallelism;
performance beyond the efficiency gate; pretty TUI output (JSON only).
