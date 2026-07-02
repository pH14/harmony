# Task 64 — `dissonance/explorer`: the search-plane trait spine + Progression refactor

> **The Wave-5 keystone contract.** Every later task (signals, matcher DSL, selector, tactics,
> oracle, triage) implements a trait defined here. This task adds the spine and refactors the
> engine onto it **behavior-preservingly** — no new faults, signals, or cleverness, just their seams.
>
> The **materialization box-gate** needs **task 63** = **GO** and **task 58** (a live `Machine`);
> the spine + refactor depend on nothing. **Coordinate with task 94**: this spec is post-rename
> (Theme→Progression, Variation→Modulation); land after 94 or fold it in — the foreman sequences.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` (the whole doc — this task *is* its
Phase C), `docs/DISSONANCE.md` ("The two loops", "Progression is agnostic-by-interface", the
reproducer/`compose` ruling), `dissonance/explorer/src/` (`engine.rs`, `strategy.rs`, `corpus.rs`,
`seam.rs`, `lib.rs` — the code being refactored), `dissonance/environment/src/` (`Environment`,
`Moment`, `EnvCodec`), `dissonance/control-proto/src/types.rs` (`StopReason`, coverage geometry).

## Environment

Pure-logic, macOS+Linux, laptop-gated — a `dissonance/` crate refactor; no box for the spine or
the behavior-equivalence proof (both run on the toy `Machine`). The parent-rooted materialization
gate is **box-only** and deferred to the frontier task — its bar, not this task's work.

## Context

The explorer (task 12) drives both loops with one `Strategy`, conflating the two responsibilities
`docs/EXPLORATION.md` separates — in-run decision answering (inner, open-loop) vs. choosing/scoring
the next run (outer) — plus an AFL-shaped `Corpus` (a global `(edge, bucket)` novelty set) that
saturates on a whole-guest workload and can't express "situations worth returning to." It
decomposes `Strategy` into **`Tactic`** (inner) + **`Selector`** (outer), generalizes `Corpus`
into an **`Archive`** of cells, and keeps the Progression as blind to fault/cell *meaning* as today.

## Public API

`dissonance/explorer/src/spine.rs`, new (crate-root re-export): interfaces defined **here, in
the consumer** (hard rule 2); later plugin crates depend on `explorer` and implement them.
Signatures fix names/roles/decomposition; parameter lists may vary where the semantics below hold.

### Vocabulary (serializable, `serde`)

```rust
/// One run, decoded and serializable — the unit the replay plane operates on.
pub struct RunTrace {
    pub terminal: StopReason,                 // control-proto
    pub env:      Environment,                // environment — the genesis-complete reproducer
    pub coverage: Option<CoverageView>,       // instrument tier, snapshotted at run end (terminal signal)
    pub events:   Vec<(Moment, GuestEvent)>,  // link tier (decoded SDK) — empty until task 73
    pub records:  Vec<(Moment, Record)>,      // scrape tier (decoded logs/spans/events) — empty until task 65
}

pub struct Feature { pub channel: ChannelId, pub id: FeatureId }  // stable id; open-vocab codebooks live in plugins
pub struct FeatureSet { /* the features live at a given Moment */ }
pub type CellKey = Vec<u8>;                                        // opaque to the Progression; Ord for BTree keying

/// A frontier entry: PARENT-ROOTED so materialization replays only the suffix (never genesis).
pub struct VirtualExemplar {
    pub parent: SnapId,        // an already-sealed ancestor (or genesis)
    pub seed:   u64,
    pub suffix: Environment,   // tail-complete delta since `parent` (compose contract, DISSONANCE.md task 93)
    pub at:     Moment,        // where to seal within the branch
}

pub struct Bug { pub env: Environment, pub stop: StopReason, pub fingerprint: [u8; 32] }
```

### Replay-plane traits — pure per run

```rust
/// Raw-observable → timestamped features. One RunTrace yields a STREAM, not a terminal set.
pub trait Sensor { fn observe(&self, t: &RunTrace) -> Vec<(Moment, Feature)>; }

/// Point-in-time feature slice → cell key. The one campaign-defined stage; downstream is generic.
pub trait CellFn { fn key(&self, at: Moment, feats: &FeatureSet) -> CellKey; }

/// Trace oracle: a pure verdict over a finished run (Crash, always-assertion, Elle-over-history).
pub trait Oracle { fn judge(&self, t: &RunTrace) -> Option<Bug>; }
```

### Replay-plane folds — stateful across the run sequence

```rust
/// The Go-Explore/MAP-Elites frontier: cells, best-per-cell exemplars, cost-based eviction.
pub trait Archive {
    /// Admit along the WHOLE timeline: one run seeds a VirtualExemplar at each novel (cell, Moment).
    fn admit(&mut self, t: &RunTrace, cells: &dyn CellFn, sensors: &[Box<dyn Sensor>]) -> Reward;
    fn admissible(&self, at: Moment) -> bool;              // injected at construction; default always-true
    fn evict(&mut self);                                   // best-per-cell; must be reproducibility-safe
    fn frontier(&self) -> &Frontier;
}

/// Outer-loop policy: which exemplar to branch from next. Generic — never sees cell meaning.
pub trait Selector {
    fn choose(&mut self, frontier: &Frontier) -> Option<ExemplarRef>;
    fn reward(&mut self, chosen: ExemplarRef, r: Reward);
}
```

### Live-plane trait — the inner loop's answering policy

```rust
/// STATEFUL DISTRIBUTION over surfaced decisions; OPEN-LOOP — never reads Sensor/CellFn/Archive output mid-run.
pub trait Tactic { fn decide(&mut self, pt: &DecisionPoint, rng: &mut Prng) -> Answer; }
```

### For the matcher DSL (task 66) to adapt any record type

```rust
pub trait Matchable { fn kind(&self) -> &str; fn attr(&self, k: &str) -> Option<Value>; fn moment(&self) -> Moment; }
```

## Semantics that must hold

1. **Open-loop `Tactic` (the load-bearing invariant).** `Tactic::decide` depends only on the
   tactic's own state, the `DecisionPoint`, and its PRNG — never on `Sensor`/`Archive` output;
   proptest: identical `(state, point, rng)` ⇒ identical answers, whatever concurrent runs do.
2. **Timeline admission.** `Archive::admit` walks the run's feature timeline and admits a
   `VirtualExemplar` at **every** novel `(cell, Moment)` — one run contributes many exemplars.
   Best-per-cell domination replaces a cell's exemplar when a dominating one arrives (shallower
   `at`, or a configured quality key); the archive is bounded by **distinct cells**, not runs.
   Admission consults the injected `admissible(at)` predicate; if task 63 rules RESTRICTED, its
   `sealable` plugs in so only materializable points are admitted — with zero spine change.
3. **Parent-rooted exemplars.** A `VirtualExemplar` is `(parent SnapId, seed, tail-complete
   suffix, at)`; materialization = `branch(parent)` + replay the suffix + seal — never a genesis
   replay. The genesis-complete `Bug.env` folds the suffix chain via `EnvCodec::compose`.
4. **Eviction is reproducibility-safe.** Dropping any exemplar/snapshot never changes what a later
   run reproduces (an evicted state re-materializes from genesis, identically); retention is a pure
   performance knob. Proptest: aggressive eviction finds the same bug fingerprints as none.
5. **Progression blindness preserved.** `Selector` and `Archive` see opaque `CellKey`s and
   `Reward`s — no fault types, signal channels, or `CellFn` meaning; later faults/signals never
   touch this module (the `DISSONANCE.md` invariant).
6. **Behavior equivalence.** The default `Selector`+`Archive`+`Tactic`, composed as the old
   `Strategy` was, reproduces the pre-refactor campaign — same bug fingerprints and admission
   decisions on the toy `Machine` across a fixed seed set: structure changes, outcomes don't.

## Prior art

- **Go-Explore** (Ecoffet, Huizinga, Lehman, Stanley, Clune — Nature 2021) [eng] — the
  archive-of-cells / return-then-explore skeleton behind `Archive::admit` + materialize-then-branch;
  its documented failure modes (detachment, derailment, doomed exemplars) are the design rationale
  for timeline admission, several-exemplars-per-cell, and best-per-cell domination — and exact
  deterministic "return" makes Go-Explore's hardest engineering problem free here.
- **MAP-Elites** (Mouret & Clune, 2015) — the quality-diversity framing for best-per-cell
  replacement (semantics item 2's domination rule).

## Acceptance gates

1. **Standard suite** green on `dissonance/explorer` (build / nextest / clippy `-D warnings` / fmt /
   deny), all-features, on macOS + Linux.
2. **Decomposition proptests (≥256):** invariants 1 (open-loop), 2 (timeline admission bounds the
   archive by cells), 4 (eviction is reproducibility-safe).
3. **Behavior-equivalence gate:** a fixed suite (≥50 campaigns × the existing toy `Machine`) yields
   byte-identical bug fingerprints and admission decisions pre- and post-refactor.
4. **Contract-only (box; deferred to the frontier materialization task):** materializing a deep
   `VirtualExemplar` replays only the suffix (≪ genesis); the state survives ancestor eviction.

## Non-goals

- Any new `Sensor`, `CellFn`, `Selector`, `Tactic`, or `Oracle` *implementation* beyond the
  behavior-equivalence defaults — tasks 65–75; this task ships the traits and the refactor only.
- The live materialization engine and coverage-shmem wiring — the frontier task after 58/63.
- Changing the reproducer/`compose` contract — it is ruled (task 93); consume it, don't revisit.
