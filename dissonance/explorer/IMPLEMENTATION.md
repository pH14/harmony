# explorer — implementation notes

Task 12 (the Timeline/Multiverse engine) plus **task 64: the search-plane trait
spine + Progression refactor** — the Wave-5 keystone contract. Pure logic: no
`/dev/kvm`, no guest, no socket, no wall-clock, no host entropy, no
sibling-crate dependencies. Builds and passes every gate on macOS and Linux. No
`unsafe`, so no Miri obligation.

## What task 64 built

- **`src/spine.rs` — the contract** (crate-root re-exports, conventions rule 2:
  interfaces live in the consumer). The serializable vocabulary — `RunTrace`,
  `Feature`/`FeatureSet`/`ChannelId`/`FeatureId`, `CellKey`, `VirtualExemplar`,
  `Bug`, `Fork`, `CoverageView`, `GuestEvent`/`Record`/`Value`, `Moment`,
  `Reward`, `Frontier`/`FrontierEntry`/`ExemplarRef`, `DecisionPoint` — and the
  traits: `Sensor`/`CellFn`/`Oracle` (replay plane, pure per run),
  `Archive`/`Selector` (replay plane, stateful folds), `Tactic` (live plane,
  open-loop), `Matchable` (task 66's record adapter). Everything a later
  signal/search/oracle task implements is defined here.
- **`src/defaults.rs` — the behavior-equivalence defaults**, the pre-refactor
  god-object decomposed: `DeclineTactic` + `GenesisSelector` (`SeedStrategy`'s
  two halves), `ExploreExploitSelector` (`CoverageStrategy::next_env`,
  draw-for-draw), `CoverageArchive` over `IdentityCells` (the `Corpus`'s AFL
  fresh-pair rule generalized to first-wins cells, with the injected
  `sealable(Moment)` predicate defaulting always-true until task 63 rules),
  `TerminalOracle` (`is_bug` as a plugin; same golden `sha2` fingerprint). No
  new search cleverness (task-64 non-goal).
- **`src/engine.rs` — the refactored engine.** `Explorer<M>` over a
  `Composition { tactic, selector, archive, oracle, cells, sensors }` and one
  campaign `Prng`. The Selector picks a frontier exemplar (or `None` =
  genesis); the engine materializes it, mints the next env through `EnvCodec`,
  runs one Timeline (the Tactic answering open-loop), rebases the run to
  genesis-complete (task-93 `compose`), folds it into the Archive (timeline
  admission over the run's `Fork`s), rewards the selector, and returns the
  Oracle's verdict. `Strategy`/`SeedStrategy`/`CoverageStrategy`/`Corpus`/
  `CovScore` are deleted; `Prng` is public (it is in `Tactic`'s signature).

### The exemplar/seal split (how `Corpus` became `Archive`)

A frontier entry is now a **parent-rooted `VirtualExemplar`** `(parent SnapId,
seed, suffix, at)` plus its memoized genesis-complete env (the suffix-chain
fold, computed by the engine at admission — the schema-blind archive never
composes). The expensive half — a live snapshot — is a separate, engine-side
**seal** cache: minted eagerly at each fork (exactly where task 12 snapshotted,
which is what keeps the refactor behavior-preserving), re-minted on demand.
`Explorer::evict_seals` drops every seal; frontier entries survive, and a later
exploit **re-materializes from genesis** (`branch(genesis, entry.env)` replayed
to `exemplar.at` under `StopMask::NONE` — a pinned replay, nothing surfaces).
Determinism makes the re-materialized state hash-identical to the evicted seal
(gated), so retention is a pure performance knob — spine invariant 4. The
suffix-only fast path (`branch(parent)` + replay ≪ genesis) is the frontier
task's box-gated mechanism (acceptance gate 4, explicitly deferred); `parent`
is recorded now so that task needs zero spine change.

## Gates (all green, macOS)

- Standard suite: `cargo build/nextest/clippy(-D warnings, --all-targets)/fmt
  -p explorer --all-features`, `cargo deny check`. 57 tests (unit +
  integration), suite ≈ 0.8 s; rustdoc builds warning-free. (Clippy still
  surfaces the three *pre-existing* workspace
  `clippy.toml` meta-diagnostics about `rand::*` paths pulled in by proptest;
  they cite no code here and do not fail `-D warnings`.)
- **Decomposition proptests** (`tests/spine_invariants.rs`, ≥256 cases each):
  - *Open-loop Tactic* — a recording tactic logs `(point, stream-state,
    answer)` inside a live campaign; replaying the log through a fresh
    instance, with a **different** campaign running between decisions,
    reproduces every answer. Structurally the engine hands a tactic nothing
    else — `decide(state, point, rng)` has no coverage/archive parameter.
  - *Timeline admission bounds the archive by cells* — entries ≤ occupied
    cells ≤ the toy's cell space, at 1× and 4× the run count; plus the
    one-run-many-exemplars witness (a single step admits at both fork
    moments).
  - *Eviction is reproducibility-safe* — a campaign evicting every seal after
    every step yields byte-identical bugs and admissions to one that never
    evicts; plus the direct witness (seal hash == re-materialized hash).
- **Behavior-equivalence gate** (`tests/behavior_equiv.rs`): 50 campaigns ×
  the *unchanged* toy machine against the **vendored pre-refactor engine**
  (`tests/reference/mod.rs`, the task-12 code frozen verbatim) — byte-identical
  bug fingerprints, bug reproducers, and admission decisions (envs + scores),
  across seed campaigns (`StopMask::ALL`, declined decisions) and full
  explore/exploit campaigns (`SNAP_BIT` only: salt-picked exploits, mutation
  minting, nested forks, compose rebasing). Draw-for-draw stream equality —
  the defaults are composed on one campaign `Prng` exactly as the old
  `Strategy` owned one.
- The task-12 gates carry over re-stated: determinism (≥256), Timeline replay +
  the task-93 `compose_rebase_replays_from_genesis` property (≥256), novelty
  order-stability (≥256), two error categories, seal GC/no-leak, nested-fork
  genesis-completeness pins, artifact equivalence across tactics.
- **Contract-only, deferred (box):** acceptance gate 4 — deep-exemplar
  materialization replaying only the suffix and surviving ancestor eviction —
  belongs to the frontier materialization task (68) per the spec's Environment
  section; nothing here runs on the box.
- Mutation testing: `cargo mutants --in-diff` over this branch's diff —
  see the bottom of this file.

## Deviations from the spec's sketch (all documented in-code)

1. **`Archive::admit` takes a `forks: &[Fork]` parameter** beyond
   `(t, cells, sensors)` (the spec allows: "parameter lists may vary where the
   semantics hold"). The replay plane cannot reconstruct sealable-point
   material from a `RunTrace` alone: the suffix-at-a-moment is emitted by the
   machine *at the fork* (`recorded_env`), and slicing `t.env` after the fact
   would be schema-aware — `EnvCodec` territory the schema-blind archive must
   not touch (task 12 already rejected a codec `slice` verb). `Fork` bundles
   the exemplar, its pre-folded genesis-complete env, and the signal view as
   of that point. When task 65 enriches `RunTrace`, sensors supply timeline
   features through the same `admit` walk — zero spine change.
2. **`Selector::choose` takes `rng: &mut Prng`**, mirroring `Tactic::decide`:
   a stochastic outer policy draws from the caller-seeded campaign stream
   (the old god-object owned exactly one stream; the equivalence gate pins the
   shared-stream draw order). `Selector::reward` is as specced.
3. **The one dropped behavior: `CoverageStrategy::choose`'s live-coverage
   fold.** The old inner-loop answer folded `checksum(machine.coverage())` —
   intra-run, closed-loop feedback, which is precisely what the load-bearing
   open-loop invariant (spec semantics 1; EXPLORATION.md invariant 1) outlaws,
   and what `Tactic::decide`'s shape now makes unexpressible. The equivalence
   suite therefore drives the pre-refactor engine in the configurations whose
   behavior survives the ruling (declined decisions / masked decisions) and
   proves byte-equality there; the fold itself has no legal post-refactor
   counterpart. Anyone needing coverage-*adaptive* answering does it the
   ruled way: between runs, via checkpoint-and-refuzz.
4. **`VirtualExemplar.seed` is the campaign draw** (the explore seed or the
   exploit mutation salt) that minted the run's environment — provenance. The
   engine is schema-blind and cannot extract the env-internal seed; the
   authoritative reproducer is `suffix`/`env` anyway.
5. **`Moment` is stamped one-for-one from machine V-times** (`Moment(vtime.0)`)
   in this crate. The spine keys on `Moment` as the spec fixes; which physical
   counter backs the axis at integration is the `Moment`-vs-`VTime` unit ruling
   EXPLORATION.md escalates to the foreman with task 65 — nothing here depends
   on the choice.
6. **`CoverageArchive` consumes coverage per sealable point** (the toy exposes
   its map live) — the faithful port of task-12's fork-time admission, which
   the equivalence gate requires. EXPLORATION.md notes production shmem
   coverage is terminal-only; when that lands, coverage feeds terminal
   admission and the along-timeline features come from sensors — an archive
   implementation detail, not a spine change.
7. **Best-per-cell is first-wins in the default** (never replaced) — the
   degenerate domination key, because replacement would change pre-refactor
   outcomes. The `Frontier` ships the domination primitive (`occupy`,
   returning the displaced ref) for task-70+ quality keys.
8. **`Bug` field order is the spec's** (`env`, `stop`, `fingerprint`) — the
   pre-refactor struct had `fingerprint` first; same fields, same fingerprint
   function (golden-pinned unchanged).

## Deviations considered and rejected

- **Keeping `Strategy` as a compatibility shim over the new parts.** Rejected:
  the spec's point is decomposing the god-object; a live conflated trait
  invites new code onto the wrong seam. The vendored copy in `tests/reference`
  keeps the pre-refactor semantics executable for the equivalence gate without
  shipping them.
- **Golden files for the equivalence gate** (dump pre-refactor outcomes,
  compare post-refactor). Rejected: goldens rot and cannot be re-derived
  without git archaeology; the vendored reference engine is reviewable,
  regenerates the baseline on every run, and pins *both* sides to the same toy.
- **Putting seals (SnapIds) in `FrontierEntry`.** Rejected: the archive is
  replay-plane — it must never hold live backend resources (the old `Corpus`
  holding `SnapId`s is part of what this refactor retires). Seals live in the
  engine; `VirtualExemplar.parent` is provenance, not a held handle.
- **A `CoverageSensor` implementation.** Rejected: with `events`/`records`
  empty until tasks 65/73 and coverage consumed by the default archive at
  forks, a sensor impl would be dead code shipped only to look complete —
  and new `Sensor` impls are an explicit non-goal. The trait is exercised in
  tests (a sensed feature admits a coverage-less fork at its moment).
- **Floats anywhere in scoring.** Never considered seriously: `Reward` is
  integer-only (`new_cells: u64`), per the Wave-5 integer/rational ruling.

## Known limitations / integrator notes

- **⚠ Task-58 conflict (PR #44), for the PR body:** task 58's socket-backed
  `Machine`/`SpecEnvCodec` bind to this crate's seams. The `Machine`/`EnvCodec`
  traits and their semantics are **unchanged** here, but (a) `Explorer::new`
  now takes `(machine, codec, Composition, seed)` and is generic over `M`
  only, (b) `Strategy`/`SeedStrategy`/`CoverageStrategy`/`Corpus`/`CovScore`
  no longer exist, (c) `RunOutcome` lost `coverage_novelty`, and (d)
  `MachineError` gained `UnknownExemplar` (breaking for exhaustive matches).
  Whichever lands second rebases: a task-58 conductor demo constructs
  `Explorer::new(machine, codec, Composition::defaults(), seed)` and swaps
  strategy names for the default composition. Per the foreman's instruction
  this branch does **not** adapt to unmerged task-58 code.
- **`GuestEvent`/`Record` are deliberately minimal** (`kind` + sorted
  `attrs: BTreeMap<String, Value>`, matcher-DSL-shaped). Task 65/73 own their
  real decode; if they need more fields, additions are non-breaking.
- **`Reward` may grow fields** (e.g. a quality magnitude for domination keys)
  — additive, anticipated by the handoff notes.
- **Terminal-coverage admission is deliberately absent** in the default
  archive: task 12 never admitted terminal states (they are not branchable
  fork points in the toy), and adding it would break equivalence. The
  `RunTrace.coverage` field carries the terminal view for future archives.
- **`Explorer::materialize` is public**: the frontier task's live
  materialization engine replaces its genesis-replay body with the
  `branch(parent)`+suffix fast path behind the same signature.
- **Naming**: engine loop names (`timeline`/`multiverse_step`) are task-12's;
  spine/docs use the post-rename Progression/Modulation framing. Task 94 does
  the tree-wide rename — deliberately not done here.
- **CI wiring left to the integrator (root files are off-limits, rule 1):**
  `explorer` is already in the `public-api` job's `-p` list; the
  `tests/public-api.txt` snapshot is regenerated on the pinned nightly
  (`nightly-2026-06-16`) and verified. No `miri` entry is needed (no
  `unsafe`). `Cargo.lock` is regenerated by the integrator (this branch adds
  the whitelisted `serde`/`serde_json` to this crate only).

## Still-true task-12 notes (unchanged by the refactor)

- **Schema-blind engine; mutation lives here, never in the wire.** The engine
  ferries opaque `Environment` blobs and mints/mutates/composes only through
  `EnvCodec`.
- **Genesis-complete vs branch-local reproducers.** `Machine::recorded_env` is
  branch-local; every admitted frontier env and every reported `Bug.env` is
  rebased genesis-complete through the entry's own genesis-complete env (now
  also the `RunTrace.env` invariant, per the spec). The prefix-env-at-the-fork
  pairing (not the whole-run env) is preserved and gated.
- **The toy machine is a faithful stand-in** (absolute-index seed answers make
  branch-local deltas recompose; the toy is the counter-mode alternative the
  task-93 ruling names, so tail-completeness is not needed *for the toy* —
  the production adapter's tail-complete contract is unchanged and binds
  task 58/68).
- **Two result categories, fail-loud**; library never panics on untrusted
  input; determinism by construction (BTree everywhere order can reach an
  output, one seeded xorshift64\*, `sha2` fingerprints).

## Mutation testing

`cargo mutants --no-shuffle --in-diff <branch diff>` (the CI `mutants` job's
exact invocation): **91 mutants tested, 0 missed** (74 caught, 17 unviable —
`Default::default()` substitutions on types without `Default`, e.g. `Answer`,
deliberately non-`Default` per the task-12 note). The golden pins
(xorshift64\* sequence, fingerprint digest, AFL bucket ranges, `IdentityCells`
key bytes, selector explore/exploit boundary, coverage-feature packing, the
per-fork seal-pairing pin) carry the load. A first pass missed six mutants;
they were closed by restructuring rather than silencing: the seal-pairing walk
lost its compensating rescan (every operator now observable), the coverage
feature id switched to arithmetic packing (`edge*256+bucket` — the `|` form's
operands never overlapped, making `|`→`^` equivalent), and `FeatureSet` gained
negative assertions.
