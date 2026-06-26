# Task 12 — `dissonance/explorer`: the coverage-guided exploration engine (policy)

Read `tasks/00-CONVENTIONS.md` first. Touch only `dissonance/explorer/`.

Design basis: `docs/DISSONANCE.md` (the control verbs this drives + the **Timeline** /
**Multiverse** loops). **Sequencing:** depends on the *contracts* of task 24
(`Environment`/`Answer`/`DecisionClass`) and task 25 (`StopReason`/verbs), but per conventions
rule 2 it **defines its driver traits locally** and tests against an in-crate toy machine — so it
is delegable in parallel; only integration waits on 24/25.

## Environment

Runs on: macOS and Linux. Requires: Rust (stable). Does **not** require `/dev/kvm`, a guest OS,
or a socket. Pure-logic exploration engine, fully gate-testable against an in-process toy machine.

## Context

This is **all of policy** — the brain that drives the two planes to find bugs. It owns the
corpus, coverage-novelty scoring, the per-run decision policy, and the mutation/scheduling
strategy. Mutation lives **here**, never in the wire (the AFL lesson).

Two loops (`docs/DISSONANCE.md`):

- **Timeline (inner):** drive ONE run forward — `run` ⇄ `run(resolve)` — answering each surfaced
  `Decision` via `policy.choose(ctx, coverage)` and **accumulating the answers as an
  `Environment`** (the reproducer). Ends at a terminal `StopReason`.
- **Multiverse (outer):** across runs — pick/mutate an `Environment` from the corpus, `branch`,
  run one Timeline, score coverage novelty + assertions, keep if interesting. One Multiverse step
  = one Timeline.

The engine codes against a `Machine`/`MachineFactory` seam (defined locally). In production a
thin R2-socket adapter implements it (frontier); in tests an in-crate **toy machine** does — so
the same engine and the determinism gate run both sides unchanged.

## Public API

```rust
// ---- the driver seam (locally defined; R2 socket adapter / toy machine implement it) ----
pub struct SnapId(pub u64);
pub struct VTime(pub u64);
/// Opaque to the explorer (task 24 owns the structure). The engine ferries & mutates bytes.
pub struct Environment { pub blob_version: u16, pub bytes: Vec<u8> }
pub struct Answer(pub Vec<u8>);

pub struct StopConditions { pub deadline: Option<VTime>, pub on: StopMask }
pub struct StopMask(pub u32);                       // mirrors control-proto / DecisionClass bits
pub enum StopReason {
    Deadline { vtime: VTime }, Quiescent { vtime: VTime }, Crash { vtime: VTime, info: Vec<u8> },
    Decision { vtime: VTime, id: u64, ctx: Vec<u8> },
    Assertion { vtime: VTime, id: u32, data: Vec<u8> }, SnapshotPoint { vtime: VTime },
}

pub trait Machine {
    fn branch(&mut self, snap: SnapId, env: &Environment) -> Result<(), MachineError>;
    fn replay(&mut self, snap: SnapId) -> Result<(), MachineError>;
    fn run(&mut self, until: &StopConditions, resolve: Option<&Answer>) -> Result<StopReason, MachineError>;
    fn snapshot(&mut self) -> Result<SnapId, MachineError>;
    fn drop_snap(&mut self, snap: SnapId) -> Result<(), MachineError>;
    fn hash(&mut self) -> Result<[u8; 32], MachineError>;
    /// The coverage map for the most recent run (AFL-style edge counts). In production this is a
    /// view of the shmem region; in the toy machine it is synthetic.
    fn coverage(&self) -> &[u8];
    /// The reproducer `Environment` accumulated over the current Timeline: the base seed/policy plus
    /// the answers resolved since the last `branch`/`replay`. The Machine owns the `Environment`
    /// backing (it mediates every `run(resolve)`), so it — not the schema-blind explorer — emits the
    /// recorded blob; the explorer ferries it into `RunOutcome`/`Corpus` without parsing it.
    fn recorded_env(&self) -> Result<Environment, MachineError>;
}
pub trait MachineFactory { type M: Machine; fn spawn(&self) -> Self::M; }
pub enum MachineError { /* transport/backend failure surfaced from ControlError; thiserror */ }

/// Mint/mutate **valid** `Environment` blobs. Task 24 owns the blob structure, so the explorer
/// stays schema-blind by going through this seam (bound at integration to `EnvSpec`'s codec; the
/// toy machine provides a trivial impl). Without it a production strategy could only emit raw bytes
/// the control backend rejects as `BadEnvVersion`/`MalformedEnvironment` — exploration would never
/// leave the toy machine. The strategy *decides* (seed / which override to mutate — that is policy);
/// the codec *encodes* (task 24's structure).
pub trait EnvCodec {
    /// A fresh pure-seeded environment (no overrides) — the `SeedStrategy` / empty-corpus base.
    fn seeded(&self, seed: u64) -> Environment;
    /// A coverage-guided mutation of `base`: decode, tweak the seed or one override, re-encode —
    /// always a *valid* blob the backend accepts, never a raw byte-flip. `salt` makes the choice
    /// deterministic (no wall-clock / host-RNG).
    fn mutate(&self, base: &Environment, salt: u64) -> Environment;
    /// Compose a genesis-complete `base` with a **branch-local** delta (a `Machine::recorded_env`
    /// from a run branched off `base`'s snapshot) into one genesis-complete `Environment`, by
    /// re-indexing the delta's decision IDs onto the end of `base`. This is how a `Bug` found below a
    /// non-genesis corpus snapshot still yields a portable, genesis-replayable reproducer. Deterministic.
    fn compose(&self, base: &Environment, branch_local: &Environment) -> Environment;
}

// ---- pluggable strategy (seed-only / coverage-guided / human all fit) ----
pub trait Strategy {
    /// Answer one surfaced decision (Timeline). `ctx` is opaque service↔policy bytes.
    fn choose(&mut self, ctx: &[u8], coverage: &[u8]) -> Answer;
    /// Produce the next Environment to try (Multiverse): mutate a corpus entry or draw a fresh seed,
    /// **minting it through `env`** so the blob is always valid (the strategy decides the seed /
    /// mutation, the codec encodes task 24's structure). On an **empty corpus** (step 1) it returns
    /// `(genesis, env.seeded(..))` — `genesis` (from `Explorer::new`) is the only valid base before
    /// anything is admitted, so it is passed in explicitly rather than hidden inside `Explorer`.
    fn next_env(&mut self, corpus: &Corpus, genesis: SnapId, env: &dyn EnvCodec) -> (SnapId, Environment);
}
pub struct SeedStrategy { /* draws fresh seeds; never overrides → pure DST */ }
pub struct CoverageStrategy { /* novelty-guided choose + mutate */ }

// ---- corpus + engine ----
pub struct CovScore(pub u64);
pub struct Corpus { /* entries: (SnapId, Environment, CovScore); a deterministic novelty index */ }
impl Corpus {
    pub fn new() -> Self;
    pub fn admit(&mut self, snap: SnapId, env: Environment, coverage: &[u8]) -> bool; // true if novel
    pub fn len(&self) -> usize;
}

pub struct RunOutcome { pub stop: StopReason, pub env: Environment, pub coverage_novelty: CovScore }

pub struct Explorer<M: Machine, S: Strategy> { /* machine, strategy, corpus, EnvCodec */ }
impl<M: Machine, S: Strategy> Explorer<M, S> {
    /// Snapshots the freshly-spawned machine at its quiescent boot point → the **genesis `SnapId`**,
    /// the base every first-generation Timeline branches from (the corpus starts empty, so step 1 has
    /// no admitted entry to branch from). Held internally and passed (with the `EnvCodec`) to
    /// `Strategy::next_env`. Returns `Err` if that initial `snapshot` fails (e.g. not quiescent) —
    /// never panics or fabricates a base.
    pub fn new(machine: M, strategy: S, env: Box<dyn EnvCodec>) -> Result<Self, MachineError>;
    /// Inner loop: drive one run to a terminal stop, accumulating the answered `Environment`.
    pub fn timeline(&mut self, base: SnapId, env: &Environment, until: &StopConditions)
        -> Result<RunOutcome, MachineError>;
    /// Outer loop: one Multiverse step (pick/mutate → branch → timeline → score/admit).
    pub fn multiverse_step(&mut self) -> Result<Option<Bug>, MachineError>;
    /// Run the Multiverse for a bounded number of steps; returns bugs found.
    pub fn explore(&mut self, steps: u64) -> Result<Vec<Bug>, MachineError>;
}
/// A reproducer. `env` is **genesis-complete**: `branch(genesis, env)` + re-run reproduces `stop`
/// bit-for-bit. Overrides are keyed by decision index *since the branch*, so a non-genesis branch
/// base would make `env` alone ambiguous — so the explorer **rebases before reporting** by composing
/// the corpus base env with the branch-local delta (`EnvCodec::compose`) into a genesis-complete env
/// (SnapIds are ephemeral pool handles, never part of a portable artifact; the genesis snapshot from
/// `Explorer::new` is the one stable, always-reproducible base).
pub struct Bug { pub fingerprint: [u8; 32], pub env: Environment, pub stop: StopReason }
```

## Acceptance gates

Beyond the standard gates in conventions:

1. **Toy machine + determinism.** Provide an in-crate deterministic toy `Machine` (a tiny state
   machine whose run answers `Decision`s and whose `coverage`/`hash` are pure functions of the
   answer sequence). Same `(strategy seed, toy machine)` ⇒ identical exploration trace and
   identical set of admitted corpus entries. Property test, ≥256 cases.
2. **Timeline replay (the OQ10 gate).** A `timeline` run accumulates `env₁` (= `Machine::recorded_env`
   at the terminal stop); `branch(base, env₁)` + re-run to the same deadline yields the **same `hash`**
   as the original run — i.e. the recorded `Environment` reproduces the run bit-for-bit. (Toy machine.)
   A reported `Bug` replays from **genesis**: `branch(genesis, bug.env)` + re-run reproduces
   `bug.stop` (the env is rebased to genesis on report; a non-genesis base would mis-key the overrides).
3. **Seed vs coverage equivalence of artifact.** `SeedStrategy` (no overrides) and
   `CoverageStrategy` both emit a replayable `Environment`; replaying either reproduces its run.
4. **Novelty scoring.** `Corpus::admit` returns true exactly when the coverage map shows a new
   edge/bucket vs. the accumulated set; admission is order-stable (deterministic novelty index,
   no `HashMap` order into the kept set).
5. **Two error categories.** A `MachineError` (backend/transport failure) aborts the step loudly
   and is never recorded as a `Bug`; only `StopReason::Crash`/`Assertion` become `Bug`s.
6. **Corpus GC.** `drop_snap` is issued for evicted entries; no `SnapId` is used after drop.

## Non-goals

The R2 socket client adapter that implements `Machine` over `control-proto` (frontier, vmm-core);
the real coverage producer (SDK-event hashing / breakpoint coverage — a later coverage task); the
`Environment` internal structure (task 24). Do not embed mutation logic in any wire type.
