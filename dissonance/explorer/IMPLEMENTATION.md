# explorer — implementation notes

Task 12 (the Modulation/Progression engine) plus **task 64: the search-plane trait
spine + Progression refactor** — the Wave-5 keystone contract. Pure logic: no
`/dev/kvm`, no guest, no socket, no wall-clock, no host entropy, no
sibling-crate dependencies. Builds and passes every gate on macOS and Linux. No
`unsafe`, so no Miri obligation.

## Task 99 — `SpecEnvCodec` made fallible on malformed reproducer blobs (`hm-5d9`)

A serialized reproducer is the artifact users pass around, load from disk, and
feed back in — untrusted by definition — so the `EnvCodec` seam must not panic
on it (conventions rule 4). The task-93 default (panic-on-defect) is now
replaced by a fallible seam:

- **`EnvCodec::mutate` / `compose` return `Result<Environment, EnvCodecError>`.**
  `seeded` stays infallible (it mints from a caller-supplied seed and decodes no
  untrusted bytes). This is the intentional public-API change; the frozen
  `tests/public-api.txt` snapshot is refreshed to match.
- **New `EnvCodecError` (`src/error.rs`)** — a `thiserror` enum with one variant
  per invariant class: `Malformed(u16)` (bad magic/version/truncation/overflowing
  length field — the untrusted-input class, carrying the declared version),
  `MisorderedChain(&'static str)` (a **per-operand** invariant: a blob whose
  capture precedes its own root, `pos < base_offset`), `NonAdjacentChain(&'static str)`
  (a **pair** invariant: `branch_local.base_offset != base.pos`, so the delta was
  not recorded off the base's snapshot — round 4), `UnsupportedComposition`
  (seed/policy mismatch, standing faults, seeded variant — mirrors
  `environment::EnvError::UnsupportedComposition`), and `Overflow` (a `Moment`
  re-key past `u64::MAX`). Every internal panic on the decode path
  (`SpecEnvCodec::require`, the `mutate`/`compose` `unwrap_or_else`/`panic!` arms)
  is gone — grep-provable: no `panic!`/`unwrap`/`expect` in non-test `src/`.
- **The complete `compose(base, branch_local)` acceptance contract** (round 4 —
  do NOT spot-fix a single hole). `compose` returns `Ok` **iff** the decoded pair
  satisfies every invariant, each with its own typed error, enforced in this order:
  (1) both operands decode → else `Malformed`; (2) each satisfies `pos >= base_offset`
  (per-operand well-formedness, checked once in `require`) → else `MisorderedChain`;
  (3) **adjacency** `branch_local.base_offset == base.pos` → else `NonAdjacentChain`;
  (4) specs are splice-compatible (both `Recorded`, equal seed/policy, no standing
  faults, delegated to `environment::EnvCodec::compose`) → else `UnsupportedComposition`;
  (5) no `Moment` re-key overflow → else `Overflow`. Two facts complete the argument:
  adjacency **implies** root ordering (`d.base_offset == b.pos >= b.base_offset`), so
  the old cross-operand root check is subsumed and removed; and base
  **genesis-completeness is deliberately not required** — the adapter generalizes
  `compose` to parent-rooted bases for the task-68 lineage fold, so requiring
  `base_offset == 0` would break materialization. The full enumeration lives on the
  `EnvCodecError` doc; `compose_ok_exactly_on_the_valid_operand_pair` (a proptest
  over arbitrary positional metadata) pins the `Ok`-iff-valid biconditional and the
  exact error per failing invariant.
- **Loud control error, never a guest bug.** `MachineError` gains an
  `EnvCodec(#[from] EnvCodecError)` variant, so the engine (`progression_step`,
  `materialize`) and any campaign caller propagate a bad blob with `?` onto the
  control-plane channel that aborts the step — it is never recorded as a `Bug`
  (only `Crash`/`Assertion` are). This preserves dissonance's two-category rule.
- **Valid blobs are byte-for-byte unchanged.** Only the error path changed; the
  slice/splice/rekey logic is untouched, so every existing round-trip/replay/
  determinism test stays green (verified). Wire format is out of scope.
- **Tests.** `tests/hostile_blobs.rs` — the `compose_ok_exactly_on_the_valid_operand_pair`
  completeness proptest (the reviewable artifact), the fuzzers (arbitrary bytes
  never panic on either `compose` operand; off-version / truncation / structural
  bit-flip → `Malformed`; `pos < base_offset` → `MisorderedChain`), and a named
  regression test per invariant class (truncation-at-every-boundary, magic
  bit-flip, wrapper + inner version skew, length-field overflow, unknown variant
  tag, per-operand capture-before-root, **non-adjacent chain gap/overlap**,
  spec-content incompatibility, rekey overflow). The four in-crate `#[should_panic]`
  codec tests became `Err(...)` assertions.

**Cross-crate propagation (done in this PR).** `EnvCodec::mutate`/`compose` are
now fallible, so `dissonance/conductor`'s consumers were updated to propagate the
error: `src/materialize.rs` (the two lineage/bug `compose` folds) rides it onto
`MachineError` via `?` — those functions already return `Result<_, MachineError>`
and `MachineError: From<EnvCodecError>`, so no new imports were needed — and the
three trusted-blob test call sites (`tests/materialize_loopback.rs`,
`tests/reseed_fold_proptest.rs`) `.expect(...)` on adapter-minted blobs. The gate
is now `cargo check --workspace --all-features --all-targets` (round-1 caught the
per-crate gate missing this — the Step::SdkStop review-gap class); the whole
workspace and `cargo nextest -p conductor` are green.

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
  runs one Modulation (the Tactic answering open-loop), rebases the run to
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

**Exemplar identity is stable across eviction (round-1 review, blocking #1).**
`ExemplarRef` is a monotonic id minted by `Frontier::insert`, never reused and
never renumbered; `Frontier::remove` is the eviction primitive (cell claims
deliberately outlive their occupant — novelty never resets). The seal cache is
keyed by that id, so a compacting `Archive::evict` (the tasks-68/70 shape) can
never re-point a seal at a different exemplar: a dead ref stops resolving
(`materialize` fails loudly with `UnknownExemplar`, never a wrong snapshot) and
the engine sweeps orphaned seals after every `evict`
(`Explorer::sweep_dead_seals`). Gated by
`spine_invariants::compacting_eviction_never_desyncs_the_seal_cache` (a
compacting mock archive: survivor keeps its original id, dead refs error, the
survivor's seal hash-matches a from-genesis re-drive, handle accounting exact
across a 12-step compacting campaign) plus the
`frontier_removal_keeps_identities_stable` unit pin (ids stable, never reused,
serde round-trip preserves the id counter).

**Seal cleanup is error-safe (round-2 review, P2).** Every cleanup path —
`evict_seals`, `sweep_dead_seals`, the modulation's leftover-pending drain, and
the post-admission transfer/drop walk — removes a handle from engine ownership
only **after** its `drop_snap` succeeds (or after it is cached under its
entry's id), so a mid-way backend failure forgets nothing and the next call
retries the leftovers. Gated by `gc::failed_seal_eviction_forgets_no_handle`
(a sabotaged stale handle aborts eviction with every mapping still cached).
The default archive's fresh-cell dedup is a `BTreeSet` (was a linear
`Vec::contains` scan per feature).

**`Prng` deserialization funnels through `new` (round-1 review, blocking #2).**
xorshift64\* has one absorbing state — zero — which `Prng::new` makes
unreachable; a derived `Deserialize` would let an untrusted `{"state":0}` blob
restore it (every draw 0 forever). The hand-written deserializer normalizes
through `new` (zero remaps to the fallback exactly as seeding does), gated by a
zero-payload test and a mid-stream round-trip test.

## Gates (all green, macOS)

- Standard suite: `cargo build/nextest/clippy(-D warnings, --all-targets)/fmt
  -p explorer --all-features`, `cargo deny check`. 78 tests (unit +
  integration, the task-58 adapter suite included), suite ≈ 0.8 s; rustdoc
  builds warning-free. (Clippy still
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
- The task-12 gates carry over re-stated: determinism (≥256), Modulation replay +
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

- **Task-58 integration (PR #44, merged; rebased onto here):** task 58's
  socket adapter (`src/adapter.rs`: `SocketMachine`/`SpecEnvCodec`) and its
  conductor loopback suite bind **only** to the `Machine`/`EnvCodec` seams,
  which this refactor leaves unchanged — the adapter and
  `dissonance/conductor` compile and pass their gates against the spine with
  **zero code changes** (the rebase resolved `lib.rs` docs/re-exports and the
  `Cargo.toml` dep union, and regenerated `tests/public-api.txt`; four
  pre-existing broken rustdoc links in `adapter.rs` module docs were fixed in
  passing). API changes downstream code would notice if it ever constructed
  the engine: `Explorer::new(machine, codec, Composition, seed)`;
  `Strategy`/`SeedStrategy`/`CoverageStrategy`/`Corpus`/`CovScore` removed;
  `RunOutcome` lost `coverage_novelty`; `MachineError` gained
  `UnknownExemplar` (breaking for exhaustive matches).
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
- **Naming**: the engine loop methods are `modulation`/`progression_step`
  (inner/outer), matching the Modulation/Progression framing in the spine and
  docs. Task 94 applied this tree-wide rename (was `timeline`/`multiverse_step`,
  Timeline/Multiverse, in task-12's code); the term of art *timeline admission*
  is the run's `Moment` axis, a distinct concept, and is intentionally unchanged.
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
exact invocation): **97 mutants tested, 0 missed** at the round-1 head (78 caught, 19 unviable —
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

---

# task 68 — lazy materialization: the engine + the spanning-ancestor retention pool

Adds `src/materialize.rs` (the `Materializer`) and wires `Explorer` onto it.
Pure logic, macOS + Linux, no `unsafe`; the live gates live in
`dissonance/conductor` (its IMPLEMENTATION.md §task 68 has the box runbook and
the one substantive finding).

## What was built

- **`Materializer`** — the mechanism between `Selector::choose` and
  `Machine::branch` (an engine mechanism, not a trait, per the
  `docs/EXPLORATION.md` ruling): the seal pool (stable `ExemplarRef` → live
  `SnapId`), the **lineage table** (`SnapId → {parent, suffix, at}`,
  `BTreeMap`, genesis-rooted, **never pruned** — the chain outlives eviction),
  and an **owner indirection** (`SnapId → ExemplarRef`) so a chain naming an
  original, since-evicted `SnapId` still resolves to the same entry's
  re-minted seal (stable ids make this exact; a dead ref can never alias).
- **Materialization**: seal-cache hit → nothing; else `branch(nearest
  RETAINED ancestor, suffix)` → `run` to `at` under `StopMask::NONE` → seal →
  record lineage. Dead intermediates are folded with `EnvCodec::compose`
  (one branch + one run, never a re-seal per hop); genesis is reached only
  when no ancestor is retained. `materialize_report` returns the depth
  accounting (`Materialization { base, base_at, at, folded, from_genesis }`).
- **The retention pool**: Agamotto cost/benefit. `modeled_cost(e)` = 0 if e's
  own seal is live, else `e.at − at(nearest retained ancestor)` (the genesis
  bound at worst). `benefit(s)` = Σ over live frontier entries of the extra
  depth they would pay were `s` evicted. `enforce_budget` evicts the
  minimum-benefit seal while over `SealBudget::of(live frontier)`,
  deterministic tie-break by `SnapId`. Integer `Moment` deltas only; no
  wall-clock anywhere near the policy.
- **The task-63 ruling (GO, grid-restricted)**, both arms' seam: exemplars
  key to observed synchronized boundaries structurally (`run(deadline) → seal`
  is the only way the engine ever mints one), an identical replay must land
  **exactly** on `at` (else `MachineError::MaterializeDivergence` — loud,
  never a mis-keyed seal), and the injected `sealable(Moment)` predicate
  (default always-true = the GO arm) gates every seal the engine takes: an
  inadmissible `SnapshotPoint` is stepped past un-sealed, an inadmissible
  materialization refuses with `MachineError::NotSealable`.
- **Chain compose (the task-58 handoff)**: `SpecEnvCodec` (and the test
  `ToyCodec`) now splice at the **relative** cut `d.base_offset −
  b.base_offset` (checked; a mis-ordered chain is refused with
  `EnvCodecError::MisorderedChain` per the task-93 never-silently-mis-key
  discipline, now typed rather than panicked — task 99) and keep the base's
  root, so lineage suffixes fold into one delta still rooted at the retained
  ancestor.
  `mutate` slices at `b.pos − b.base_offset`. Genesis-complete bases are the
  `base_offset == 0` special case — v1 behavior byte-identical.

## Deviations considered and rejected

- **Folding the full lineage chain for the genesis worst case** — rejected:
  the frontier entry's memoized genesis-complete `env` is exact by
  construction (it was composed at admission) and cheaper; the fold is used
  only when a *retained non-genesis* ancestor exists.
- **Probing the machine's V-time in `Explorer::new` to learn `genesis_at`** —
  rejected: the extra `run` would shift the toy's injected-fault counters and
  the behavior-equivalence pins. `genesis_at` defaults to `Moment(0)` (exact
  for the toy) and is policy-only (it scales the cost model's genesis bound,
  never correctness); a live driver records the probed origin via
  `set_genesis_moment` / `Materializer::new`.
- **The RESTRICTED arm's precision-miss bookkeeping** (record, zero `Reward`,
  drop, continue) — not implemented: the ruling was GO. The seam is kept
  (predicate + `NotSealable`), so RESTRICTED plugs in with zero spine change;
  under GO a seal failure at an admissible point propagates loudly for
  escalation (a task-41/63 regression class), exactly per spec.
- **Pruning the lineage table** — rejected: entries are kilobytes, and
  pruning a dead intermediate would strand its descendants on the genesis
  worst case forever.

## Round-1 review fix: the wire coordinate frame (blocking, codex-found)

`SocketMachine::branch` used to ship the blob's inner `EnvSpec` verbatim —
override keys **relative** to the blob's origin — while `ControlServer`'s
task-59 contract validates and applies host faults at **absolute** Moments;
a host fault under a parent-rooted fold mis-keyed on the wire (the seed-only
task-68 gates could not see it). Fixed at the single conversion point:
`branch` re-anchors blob-frame keys at the branched snapshot's capture moment
(`origin + relative`, checked — an overflowing rebase is refused before any
wire traffic), and the frame convention is now settled **authoritatively in
one place** (`adapter.rs` module doc, "Coordinate frames": blob frame =
relative, all `EnvCodec` seams + `recorded_env`; wire frame = absolute;
`branch` outbound / `recorded_env` inbound are the only conversions).
Pinned three ways: the exact wire bytes (a captured-stream adapter test:
relative 5 below a snapshot at 200 ships as Moment 205, and the recorded
delta re-emits relative 5), a `materialize_loopback` case applying a
`CorruptMemory` below a parent-rooted fold on the real server wire (effect
observed + the compose-folded reproducer re-anchored from the *base's*
origin replays bit-identically — origin-independence), and the
rejected-behind-snapshot regression (the raw pre-fix shape is refused
`PerturbPastMoment`, never silently mis-applied).

## Known limitations / integrator notes

- **`MachineError` gained `NotSealable` and `MaterializeDivergence`** —
  additive, but exhaustive matches downstream must grow arms. The public-api
  snapshot is refreshed (all task-68 additions, nothing removed).
- **The default budget is `SealBudget::Unbounded`** (behavior-preserving:
  gc/engine-pin tests count live handles). Campaigns opt in via
  `set_seal_budget`; `progression_step` then enforces it every step.
- **`enforce_budget` is O(seals × frontier × chain-depth) per eviction** —
  fine at current scales; a cached-cost incremental version is a later
  optimization, not a semantics change.
- **The compose-fold is bit-exact on the real substrate only over
  entropy-draw-free collapsed intervals** — the substantive live finding,
  demonstrated and pinned portably in `dissonance/conductor`
  (`tests/materialize_loopback.rs`, splice pin) and written up in conductor's
  IMPLEMENTATION.md §task 68. Escalated, not patched (vmm-core is read-only
  for this task).

---

# IMPLEMENTATION — task 78 (reseed markers through the adapter frames)

Three additions in `src/adapter.rs`, all following the settled "Coordinate
frames" doc (the single conversion point discipline):

- **`rebase_to_wire`** re-anchors reseed markers exactly like overrides
  (blob-frame relative key → `origin + relative`, checked overflow).
- **`SocketMachine::branch`** records the branch reseed into the blob frame:
  a no-marker env made the server reseed from the env's seed at the restore
  origin, so the adapter stamps `record_reseed(0, seed)` into the new
  Modulation — the emitted `recorded_env` delta is then reseed-aware and a
  later fold re-executes the reseed at the collapsed hop's position. A
  marker-carrying env's own markers ride through verbatim (the server honored
  exactly those; no extra stamp).
- **`SpecEnvCodec::mutate`** slices markers at the relative cut consistently
  with overrides; `compose` splices via the underlying `environment` codec.

Known limitation: the session-initial spec handed to `connect` remains
override- and marker-free (v1 boots are), per the frame doc's deliberate edge.

# tasks/131 — campaign evidence retention + completeness policy (hm-5sv)

Replaces the old unconditional-record-only runbook rule with the strategy's
explicit retention contract (`docs/DISSONANCE-STRATEGY.md`, "Evidence retention
needs an explicit bounded policy separate from archive admission"). New module
`src/retention.rs`; the ledger (`src/ledger.rs`) gains the proof-gated GC half;
the campaign controller folds every committed batch into one deterministic set
of **retention views**.

## The three records, kept distinct by type

1. **Immutable evidence ledger** — unchanged authority (`EvidenceLedger`),
   now with proven physical downgrade (below).
2. **Versioned bounded working-set membership** — `WorkingSet`: admissions and
   expirations are ordinary positive/negative `WorkingSetUpdate`s in a
   deterministic log; expiry follows the profile's declared `ExpiryOrder`
   (`OldestFirst`: lowest `(admitted-at issue, batch id)` first — the stable
   tie-break, in the config, never an implementation accident).
3. **Committed Entry cell assignments + finalized summaries** —
   `CellAssignment` (cell, seal batch, cut, quality, **genesis-complete
   reproducer + lineage `RunId`**) and `FinalizedSummary` (monotone counters;
   no decrement API exists anywhere).

`RetentionViews::fold_batch` is THE deterministic fold: the live `step()` calls
it per committed batch, and `RetentionViews::rebuild` (checkpoint base + the
retained ledger suffix in canonical `(issue, batch)` order) replays the same
fold — so "rebuild matches live" is bit-identical **by construction**, not by
parallel reimplementation. The operational occupancy stays in lock-step with
the committed assignments (same strictly-greater-quality rule; bound by a
`debug_assert` in `step()` and by the rebuild equality gate).

## Ledger format v2 + proof-gated GC

- Frames now carry a tagged `LedgerRecord`: `Evidence | Tombstone | Checkpoint
  | Finalized`. **v1 files are rejected loudly** (`UnsupportedVersion`) — the
  v1 format merged in #130 and predates any integrated deployment; campaign
  ledgers are per-campaign artifacts, so no migration path is warranted.
- `collect(id, protected)` — the only way raw evidence leaves the authority —
  proves, in order: the batch is retained (`UnknownBatch`), a durable
  checkpoint covers its issue **or** the campaign is finalized (`NotCovered`),
  and its reproducer digest is not `protected` by a live Entry
  (`LiveEntryReference`). The tombstone (exact completeness/loss metadata,
  incl. the `CoverageRef` cited) is fsynced **before** any in-memory downgrade.
- `compact()` physically reclaims file bytes: crash-safe rewrite (temp file +
  fsync + atomic rename) that preserves the finalized marker, the rebuild
  checkpoint, every tombstone, and all retained evidence.
- **Loud exhaustion**: an optional declared byte budget
  (`CampaignConfig::evidence_budget` → `EvidenceLedger::set_budget`) fails an
  over-budget evidence append with `LedgerError::Exhausted` *before any state
  change*. Judgment call: tombstone/checkpoint/finalized appends are exempt
  from the budget — refusing them could block the explicit recovery that
  reclaims space, while admitting them can never silently change policy.

## Acceptance-criterion → test map

| Invariant | Test |
|---|---|
| `CampaignConfig` carries profile + stable tie-breaks; full-retention records from the first rollout | `retention::tests::full_retention_records_from_the_first_rollout` (+ `default_profile_is_full_retention`, `bounded_expiry_is_oldest_first_with_stable_tiebreak`) |
| Bounded expiry updates only working views; cannot retract a live Entry cell or a finalized metric | `retention::tests::bounded_expiry_updates_only_working_views` (per-step monotonicity + ledger/occupancy untouched); `full_profile_never_retracts`, `zero_cap_retracts_immediately` |
| A ledger/live-Entry-reachable reference cannot be invalidated; GC proves reachability + checkpoint coverage first | `retention::tests::gc_proves_reachability_and_coverage_before_collecting`; ledger-level `collect_requires_coverage_and_refuses_protected_references`, `shared_payload_survives_collecting_one_referent`, `retention_cannot_delete_a_live_reference`, `finalization_permits_collection_without_checkpoint` |
| Reports state exactly which raw evidence, derivations, and future recomputation remain | `retention::tests::report_states_exactly_what_remains` |
| Disk pressure cannot silently change policy — exhaustion fails loudly | `retention::tests::exhaustion_fails_loudly_never_downgrades` (campaign level); `ledger::tests::exhaustion_is_loud_and_changes_no_policy` |
| Rebuild from a supported checkpoint matches live state (bit-identical) | `retention::tests::rebuild_from_checkpoint_matches_live_state` (mid-campaign checkpoint, live suffix, GC+compaction, real file reopen, resumed campaign; + profile-mismatch refusal) |
| Same-seed retention artifacts identical (determinism gate) | `retention::tests::same_seed_yields_identical_retention_artifacts`; proptest `bounded_working_set_holds_cap_and_determinism` (256 cases) |
| GC leaves a rebuildable checkpoint or an explicit end to reinterpretation | coverage proof above + `retention::tests::finalized_collection_ends_reinterpretation` (rebuild after finalized-uncovered collection refuses, typed); `ledger::tests::compaction_reclaims_bytes_and_replays_identically` (the anchor survives compaction) |

## Deviations / judgment calls (for review)

- **`CompletedRunEvidence` gains `role: EvidenceRole` (Rollout | Seal).** The
  strategy requires rollout vs materialized-seal records to be distinguishable
  ("one search step may submit a completed rollout at one revision and its
  later materialized seal at another"); without it the rebuild fold would need
  a cut-length heuristic. This changes evidence canonical bytes → batch ids;
  no golden pins existed and no other crate constructs the type.
- **`DifferentialCampaign::new` now returns `Result<_, CampaignError>`** (was
  `MachineError`): construction rebuilds the retention views from the durable
  ledger (resuming a reopened ledger for free) and rejects a checkpoint taken
  under a different declared profile (`ProfileMismatch` — a policy change must
  be a new campaign configuration).
- **Restart occupancy restore**: committed assignments re-enter the
  operational archive with `parent = genesis` and the entry's genesis-complete
  reproducer as suffix (snapshots are ephemeral by design; first exploit
  re-materializes from genesis — the existing graceful worst case). The
  step-time `seed` draw is a diagnostic, not part of the committed record, so
  a restored exemplar carries `seed = 0`.
- **Public API snapshot regenerated** (`tests/public-api.txt`): the retention
  module surface, `EvidenceRole`, the two `CampaignConfig` fields, ledger GC
  methods, and the `new` signature change — all intentional, listed above.
- **Rejected: separate retention crate.** The policy governs the ledger and
  campaign records merged here in #130; rule 1 keeps it in `dissonance/explorer`.
- **Rejected: budget-triggered auto-GC.** The acceptance criteria explicitly
  forbid disk pressure changing policy; exhaustion aborts, an operator (or a
  new configuration) frees space via the *proven* `collect_expired`/`compact`.

## Known limitations / integrator notes

- **Resuming live search across restart needs the coordinator's durable
  ledger** (its `FileLedger`), so issue coordinates continue monotonically.
  The tests resume views/occupancy over a reopened evidence ledger but do not
  step a resumed campaign against a fresh `MemLedger` coordinator (issues
  would re-collide at 1). The evidence-ledger half is restart-complete.
- **`AbsenceLedger` is now serde** (part of the checkpointed views), via a
  `pair_map` vec-of-pairs codec because `ObservationId` is a structured key.
- The in-crate `TraceStore` remains the stand-in payload backing; when the
  real store arrives, `PayloadRef{digest, format_version}` and the
  `live_references`/`retain` seam are the contract to preserve.
- Working-set admission covers **every** committed batch (rollouts and seals);
  the bounded cap is over batches, not bytes. A byte-denominated working-set
  policy would be a new declared profile variant, not a reinterpretation.

# tasks/144 — seal-past-rollout-terminal event truncation (hm-aqf0, T136-J5)

## The bug

The marker-clamped run-forward (task 136 / PR #138) can seal a candidate
**past** the rollout terminal, at the first fully-drained quiescent boundary
beyond a staged marker. The seal's server-stamped cut counts the SDK events
fired in that advanced span, but the seal batch (a) reused
`rollout.normalized` — which stops at the terminal — and (b) contributed **no**
event rows to the graph (`evidence_rows`'s `Seal` arm returned
`events: Vec::new()`). So the seal cut had `sdk_events > graph rows`: the
cells/observation map at the seal deterministically **omitted** the advanced
span, and did so **silently** — the composed-map oracle and the Differential
relations agreed (both truncated), so no assertion fired.

## Fix — direction (a): capture the run-forward suffix

Chosen because the suffix is already normalized and reachable at the seal site
(it rides the same recorded env; the spec's stated precondition for (a)). The
seal now models what it physically is: a **continuation of the sealed rollout
past its terminal**.

- `materialize_candidate` re-reads the machine's raw SDK capture at the seal
  and decodes the **suffix** by skipping the rollout's whole raw capture
  (`rollout.raw_len` raw positions). Because the seal re-runs the identical
  branch, its raw prefix through the terminal is byte-identical to the
  rollout's, so this keeps exactly the advanced span (empty if it did not
  advance). Skipping by the rollout's raw-capture **length** (not a firing
  count) sidesteps the catalog-position offset that
  `decode_child_suffix`/`sdk_events` counts already carry.
- The seal batch is anchored on the **rollout terminal cut** (`observed_cut`)
  as its `parent_cut`, and carries the suffix as its `normalized`. Lineage
  composition (`compose_observations_at`) then walks the sealed rollout's full
  events (via `rollout.parent = rollout_id.issue`) and appends this suffix at
  cumulative positions continuing them — the shared prefix is contributed once,
  by the rollout batch, never duplicated.
- `evidence_rows`'s `Seal` arm stages the suffix state events at those same
  cumulative positions (extracted into the shared `state_event_rows` helper the
  `Rollout` arm now also uses), so the Differential graph and the composed
  oracle carry the advanced span identically.

Result: for an advanced seal the span is **present** in both the graph and the
oracle; for a non-advanced seal the suffix is empty and the batch reduces to the
prior (correct) terminal-state cell — bit-for-bit unchanged.

## Why this is hash-neutral

The change touches only evidence/graph composition (what the seal batch records
and stages), never the RNG or the schedule stream. The existing bit-identical
proptests confirm it: `explorer::same_seed_yields_identical_campaign`,
`retention::same_seed_yields_identical_retention_artifacts`, and
`campaign-runner::determinism_proptest
branch_run_hash_is_deterministic_and_replay_reproduces_capture` all stay green.

## Recomputability (the hm-efs contract direction)

`evidence_rows` is a pure function of the committed seal batch, and the seal's
suffix + terminal-anchored `parent_cut` are durable in the ledger, so a restart
re-stages byte-identical inputs and `compose_observations_at` recomputes the
seal cell from committed batches alone. The regression test's
`assert_view_parity` recomputes every seal cell from the ledger and asserts it
equals the live Differential view — the recomputability check for the advanced
seal.

## The toy had to become faithful first

The pre-144 `ScriptedMachine` surfaced **every** emit during an open-loop
rollout regardless of the terminal, so the rollout always already carried the
advanced span and the truncation could never be expressed portably. The
`run` no-deadline arm now stops at the terminal and does not capture emits
past it — the faithful model (a real rollout ends at its terminal). This is
inert for every pre-existing test: all their programs place every emit at or
before the terminal, so their trajectories are unchanged. The change is what
lets the regression test express the bug on the laptop tier.

## Acceptance-criterion → evidence

- **Regression test pinning the truncation shape:**
  `advanced_seal_captures_its_run_forward_suffix_into_the_graph`. RED before
  the seal-composition fix (`obs.get(&reg2)` is `None` while the seal cut
  claims `sdk_events == 2`); GREEN after (the advanced `reg2=7` is present, the
  graph and oracle agree, the seal re-materializes bit-identically, and the
  trajectory is same-seed deterministic). Verified red-before by reverting only
  the seal-evidence changes with the toy faithfulness kept.
- **`evidence_after_the_seal_cannot_influence_an_earlier_cell`** updated to read
  the seal cell through the lineage-composed oracle (`compose_observations_at`)
  instead of the self-contained `observations_at_cut()`. The seal's `normalized`
  is now its (here empty) suffix — consistent with branch-child rollouts, which
  already carry suffix-only `normalized` — so `observations_at_cut()` (a
  suffix-local reduction with no ledger) no longer reflects a seal's composed
  state; the composed oracle is the correct accessor and the test's invariant
  (post-seal evidence excluded by the half-open cut) is unchanged.
- **Full portable gates green** for `explorer` (142 tests) and downstream
  `campaign-runner` (179 tests): build + nextest + clippy(`-D warnings`) + fmt,
  plus workspace `cargo deny` (advisories/bans/licenses/sources ok). The two
  pre-existing `clippy.toml` config warnings (unreachable `rand::*` disallowed
  paths in crates without `rand`) are on `main` and do not fail the gate.

## Deviations considered and rejected

- **Direction (b) — refuse/drop seals past the terminal.** Rejected: the
  advanced seal is a legitimate, determinism-verified frontier entry (task 136
  went to real lengths to *keep* it and re-materialize it bit-identically);
  dropping it would forfeit reachable coverage. Capture violates no
  ledger/evidence invariant, so (a) is preferred exactly as the spec directs.
- **Keeping the full run to the seal in the seal's `normalized`** (parent_cut
  left at the base branch). Rejected: it double-counts the shared prefix once
  the graph also stages the seal's events (the graph keys events by rollout, and
  the rollout batch already staged its prefix), so a suffix-only `normalized`
  anchored on the rollout terminal is the only representation consistent across
  both the composed oracle and the Differential relations.

## Scope / known limitations

- Diff is truncation-scoped to `dissonance/explorer/` (campaign seal path +
  `testkit` + tests). Did **not** take on hm-btht's admission-time reseal/retry
  capture family or hm-kyy5's genesis-rooted re-materialization.
- Only **reducible-state** suffix events reach the graph (the cells/observation
  map is state-only, matching the `Rollout` arm). Occurrence/assertion events in
  the advanced span follow the unchanged occurrence path, which — as before —
  runs on rollout batches, not seals; that is outside this truncation's surface.

## PR #147 tribunal round — F1 (P1) + F2 (ride-along)

### F1 — advanced-seal suffix reachable to descendant recomputation

The first fix captured the advanced span correctly for the seal's own cell and
one **single** step, but stopped one lineage generation short. The seal stages
its suffix into the live Differential relation under the **sealed rollout's**
key, so a descendant that forks from the seal (an exploit child, whose
`parent_cut` is the seal cut and whose lineage parent is the sealed rollout)
inherits the advanced span through the rollout's cumulative aggregate — the live
graph is correct. But `compose_observations_at` — the direct-recomputation
oracle and the retention fold's cell authority — walked ancestors filtering
`EvidenceRole::Rollout` only, and the advanced positions `[rollout_terminal,
seal_cut)` live in **no** Rollout batch. So a descendant's ledger-recomputed
cell dropped the span (`{reg1,reg3}`) while the live view carried it
(`{reg1,reg2,reg3}`) — the PR's own `assert_view_parity` fails the moment a
descendant of an advanced seal exists (`ExploreExploitSelector`, campaign-runner's
live SelectorV1 path, triggers it).

Fix: the ancestor walk now, when a fork reaches **past** an ancestor rollout's
own terminal (`upper > anc.cut.sdk_events`), also picks up the run-forward
suffix of the Seal batch that advanced that rollout to the fork
(`role == Seal && parent == ancestor && cut.sdk_events == upper`), positioned at
`[anc_terminal, fork)`. Pushed before the ancestor so the root-first reversal
orders `anc` events then the suffix — contiguous, disjoint cumulative positions
that mirror exactly how the live relations hand the staged suffix to
descendants. A collected seal contributes nothing, like any collected ancestor
(the existing GC tolerance). The seal-composing-**itself** case is untouched
(there `upper == anc.cut.sdk_events`, not `>`).

Regression: `exploit_child_of_an_advanced_seal_recomputes_to_the_live_view` —
the judge's repro shape adapted to the codebase (a **≥2-event** advanced span
with **distinct** registers, so neither the toy-frame off-by-one nor
value-identical `Set` firings can mask the missing row), an `ExploreExploitSelector`
exploit step, and `assert_view_parity` as the compose-vs-live oracle. Red before
the walk change: `cut observations diverge (rollout 3, count 5)`.

### F2 — reconcile the capture against the stamped cut

The seal decode accepted the host's raw capture with no check that it accounts
for the stamped cut. A short or count-divergent capture would silently recreate
`cut.sdk_events > graph rows` — the precise shape this task fails closed on. The
snapshot path now reconciles the captured suffix against the stamped cut and
refuses a mismatch with the typed `MachineError::SealSuffixDivergence` — the
materializer's loud-divergence discipline (`cut_divergence_is_loud`,
`materialize_divergence_is_loud`). No honest trigger; the guard is for a
divergent host.

**The frame correction (PR #147 verify event, V1 — CONFIRMED P1).** The first
form of this check was wrong: it computed `cut.sdk_events − observed_cut.sdk_events`
across two frames. The production server stamps `cut.sdk_events` as
`vmm.sdk_events().len()` — raw capture positions, **catalog included**
(`control.rs`) — while `observed_cut` counts `normalized.events`, from which
`decode_binary` **excludes** the catalog. So an honest Binary-ingress host was
refused at every at-or-past-terminal seal (the catalog offset made
`captured != stamped − terminal`). The toy suites masked it only because the toy
stamped a firings-only count (the F6/hm-udgn frame divergence).

Closure (the preferred one): reconcile in **one frame**. The toy `snapshot()`
now stamps the production catalog-inclusive capture-position count
(`testkit.rs`: `1 + included`, folding **hm-udgn** / F6 — the toy is now
faithful to the frame `campaign-runner`'s `DeclaredMachine` and the real server
already use), and the check is `suffix.events.len() ==
cut.sdk_events.saturating_sub(rollout.raw_len)` — both operands are raw
capture-position counts, so their difference is exactly the advanced-span firing
count (`saturating_sub` gives 0 for an interior seal). `RolloutFrame` is gone;
`materialize_candidate` takes `rollout_raw_len` directly. Every stamped cut in a
toy test shifts by +1 (the catalog now counts); the three explicit-count
assertions were updated and the whole graph/compose/DD flow is transparent to
the shift (position 0 is the empty catalog slot). `campaign-runner` was already
in this frame, so its 179 tests are untouched.

Regressions (the verify event mandated both halves):
- `an_honest_production_frame_seal_capture_is_accepted` — a wrapper stamping the
  literal `inner.sdk_events().len()` (catalog-inclusive) is admitted; `step()`
  **succeeds**. This is the shape the frame-crossing bug wrongly refused.
- `a_seal_capture_short_of_its_stamped_cut_is_refused_loudly` — the same wrapper
  stamping one event beyond its capture is refused with the typed divergence.

**V2 (ride-along, P2).** After `snapshot()` succeeds, every post-snapshot
failure path — `recorded_env`, `sdk_events`, `decode_child_suffix`, the
divergence return — now releases the held seal best-effort
(`let _ = self.machine.drop_snap(seal)`) before propagating, matching
`materialize.rs`'s release-first discipline; the aborting `step()` would
otherwise leak the backend snapshot. The capture+reconcile body is split into
`capture_seal_suffix` so the single release site wraps all of them. The
divergence regression asserts a `drop_snap` actually fired (a shared counter on
the wrapper). New public error variant → `tests/public-api.txt` regenerated on
the pinned nightly.

### Scope

F3 was refuted (checkpoint coverage + step-atomicity keep a seal and its rollout
inseparable across GC). **F6/hm-udgn is folded** by the V1 closure (the toy frame
is now aligned to production). F4/F5 and V3/V4/V5 remain parked as beads per the
adjudications and are **not** touched here.

## Task 148 — ledger `VERSION` 2→3 for the suffix-only Seal representation (`hm-j7ie`, PR #147 F5 + verify V4)

### The problem this closes

PR #144 (`hm-aqf0`, the F5/V4 finding above) changed the *meaning* of a durable
**Seal** record — a Seal now serializes the run-forward **suffix + observed cut**
(`campaign.rs` `seal_suffix` / `parent_cut: Some(observed_cut)`), where a
pre-144 Seal serialized the full rollout `normalized` + base-branch `parent_cut`
— **without bumping the ledger `VERSION`** (it stayed `2`). That violates the
ledger's own doctrine (a header is "rejected against, never silently
reinterpreted"). Two concrete harms:

- A pre-144 ledger's advanced seals **reopen with historically truncated cells,
  silently** — the exact `cut.sdk_events > graph rows` silent-wrong the F1/F2
  fixes fail closed on elsewhere.
- The seal's **batch-identity preimage** (`CompletedRunEvidence::canonical_bytes`
  → `EvidenceBatchId`) differs across the upgrade for the same seed (verify V4),
  so any cross-version identity/commit-conflict comparison is meaningless.

### The ruling (foreman default; flagged for Paul's veto at review)

**Bump `VERSION` 2 → 3 and REFUSE every pre-3 ledger loudly** through the
existing `LedgerError::UnsupportedVersion` path, whose message now names the
reason (the suffix-only Seal representation change of task 144 and the
truncation a silent reinterpretation would cause). No silent fallback, no
read-old path, no migration. `VERSION` is a **private** const (`ledger.rs`), so
this is not a public-API change — see the snapshot note below.

### Refuse-vs-accept trade-off (the decision this bead owns — weigh at review)

- **Refuse (chosen).** Fail-closed is this codebase's standing doctrine for a
  durable-record meaning change with no migration demand. It makes both harms
  impossible: a v2 seal is *never decoded*, so it can neither resurrect
  truncated cells nor have its stale identity compared against a v3-computed
  digest. Cost: an operator holding a pre-144 campaign ledger cannot reopen it
  with this build — but a campaign ledger is a **per-campaign artifact that
  predates any integrated deployment**, so there is no installed base to
  migrate, and the loud refusal names exactly why.
- **Accept (rejected).** Reading old ledgers would either (a) silently reopen
  truncated cells — literally the F5 finding — or (b) require a *verified*
  seal-shape migration (rewrite each v2 Seal's `normalized`/`parent_cut` into
  the suffix frame, re-deriving `observed_cut`). No migration demand exists
  today, and building an unverified one to satisfy a hypothetical is more
  silent-wrong risk than the refusal it replaces. **If a migration is ever
  wanted it is its own future task**; per the spec this task must not build one.

### Reopen-boundary surfaces checked (spec requirement 4)

Grepped `explorer` + `campaign-runner` for anything that persists or compares the
ledger-header `VERSION` or an evidence `canonical_bytes`/`EvidenceBatchId` across
a reopen; none assumes version-2 shapes stay readable:

- **Ledger-header `VERSION`** is confined to `ledger.rs`. Both writers — `open`
  (fresh-file header) and `compact` (in-place crash-safe rewrite) — stamp the
  *current* `VERSION`; `compact` only ever runs on an already-open (therefore
  v3) ledger, so it can never re-emit a stale version. The sole reader is the
  `open` check (`found != VERSION` → `UnsupportedVersion`). No other module or
  crate embeds or compares it (`campaign-runner` does not reference it).
- **Evidence batch identity** (`EvidenceBatchId::digest(&ev.canonical_bytes())`)
  is **recomputed from each record's own bytes on every replay and append**
  (`ledger.rs` `apply`/`append`, `testkit.rs`), never persisted-as-a-value and
  compared across a reopen. The `EvidenceBatchId`s that *are* persisted
  (tombstone `CollectedBatch.batch`, retention checkpoints) live inside a single
  ledger version, and a pre-3 file is refused **before any record is decoded**,
  so no v2 identity is ever read and compared to a v3-computed digest — the V4
  cross-version-identity class is closed by the refusal, not by a compare.
- The `canonical_bytes` methods in `retention.rs` (`RetentionViews` /
  `RetentionCheckpoint` / `RetentionReport`) are a **separate** digest surface
  (retention-view identity), unaffected by the Seal representation.
- Out of scope and unchanged: `ADAPTER_BLOB_VERSION`, `EnvSpec::BLOB_VERSION`,
  `Reproducer::blob_version`, `REPRODUCER_FORMAT_VERSION` — these version
  independent blob/wire formats, not the ledger header (spec scope fence: no
  wire-format changes outside the ledger header).

### Acceptance-criterion → test map

- *A version-2 header is refused with the new message* →
  `ledger::tests::version_two_ledger_is_refused_with_the_suffix_reason`
  (asserts `UnsupportedVersion { found: 2 }` **and** the Display names `suffix`,
  `truncated`, `task 144`).
- *A freshly written ledger reopens cleanly at version 3 (round-trip)* →
  `ledger::tests::fresh_ledger_is_version_three_and_round_trips` (asserts the
  on-disk header version byte is `3` and the batch survives reopen).
- *The existing restart-rebuild suite stays green* →
  `campaign::tests::restart_rebuilds_canonical_inputs_from_the_ledger`,
  `ledger::tests::append_survives_reopen`,
  `ledger::tests::compaction_reclaims_bytes_and_replays_identically` — all pass
  unchanged. The pre-existing `foreign_version_is_rejected` (v1) also stays green
  (`1 != 3`).
- *Within-version determinism untouched* → `explorer`
  `same_seed_yields_identical_campaign` / `same_seed_yields_identical_retention_artifacts`,
  and `campaign-runner` `determinism_proptest::branch_run_hash_is_deterministic_and_replay_reproduces_capture`,
  `campaign_replays_bit_identically`, `maze_campaign::same_seed_and_config_yield_identical_artifacts`,
  `reseed_fold_proptest::draw_carrying_folds_are_bit_identical` — all green.

### Gates run

`cargo build -p explorer`; `cargo nextest run -p explorer` (147 pass, 1 box-skip)
and `-p campaign-runner` (179 pass, 1 slow, 1 skip); `cargo clippy -p explorer`
and `-p campaign-runner --all-features --all-targets -- -D warnings` (exit 0; the
only output is a **pre-existing** root `clippy.toml` config diagnostic about
`rand::thread_rng` reachability, not a lint on this change); `cargo fmt --check`.
`cargo deny` is **N/A** — no dependency change. No `unsafe` added → no Miri
obligation. Public-API is **unchanged**: `VERSION` is private, and the edit to
`LedgerError::UnsupportedVersion` touches only its Display string, not the
variant/field signature `cargo public-api` records — the ignored
`public_api_matches_snapshot` gate was run on the pinned nightly
(`nightly-2026-06-16`) and matches `tests/public-api.txt` with no diff.

### Scope fence

`hm-wshf` (accessors — unblocks automatically on this merge), `hm-mmkf` (fold
routing), `hm-4gaw`, `hm-f82p` are **not** touched. The change is the ledger
header alone: the `VERSION` const, its module/const/error docs, and the two new
regression tests.

## Task 146 (hm-whoo) — complete the seal-capture reconciliation

PR #147's `capture_seal_suffix` constrained only the **decoded suffix length**,
leaving two verified holes. This task closes both at the one choke point, in the
two halves the complete honest invariant needs.

> **PR #150 REQUEST_CHANGES (tribunal, Fable-5 judge) — applied.** The discovery
> pass rejected the first attempt's `cut.at`-bounded count form (JC1) and
> confirmed two findings against it; this section documents the **ruled** fix. See
> the "Superseded first attempt" note at the end for what changed and why.

**Count half — closes C1 (the re-check appendix, judge-CONFIRMED P1, repro at
`7f7bbda4`).** The old check compared `suffix.events.len()` against
`cut.sdk_events.saturating_sub(rollout_raw_len)`. Below the baseline the
`saturating_sub` clamped the expectation to 0, and any capture ≤ baseline decodes
to an empty suffix, so **any** `(stamp ≤ baseline, capture ≤ baseline)` pair
passed — an under-stamp silently excludes a captured firing from the sealed cell,
an over-stamp silently includes inherited rows the sealed state never reached. The
fix is the spec's ruled literal invariant, compared **before decoding**:

```
cut.sdk_events == raw.len()
```

The server stamps the SDK capture vector's current prefix length from the same
stopped state as one atomic observation (`control.rs:936-943`), so no honest
machine returns a record its clock has not reached — the stamp equals the raw
length exactly. Comparing the **length** (not a moment-derived count) is load
bearing: `decode_child_suffix` slices by raw **position**, so a record appended
past the cut moment stages as a committed suffix row regardless of its `Moment` —
a moment-bounded count counts it as absent while the decode stages a phantom row
(finding F1). The length refuses it outright.

**Content half — closes V3 (the verify event) + F2 (judge-CONFIRMED P2
ride-along).** When the seal reached or passed the rollout terminal (`raw.len()
>= rollout_raw_len`, so a run-forward suffix is composed), the shared prefix it
composes onto must reproduce the rollout's committed evidence. The seal re-decodes
its own prefix skipping the same `inherited` ancestor positions and compares it
**structurally** against the rollout's `Normalized` (`prefix !=
*rollout.normalized`). `Normalized`'s `PartialEq` covers schema, every event's
`ObservationId`, and the commitment — strictly stronger than the commitment digest
alone, which folds only each event's `Moment` and raw bytes and so is **blind to
an id/schema swap** at identical payload (F2). This is anchored on the existing
`Normalized` — **no new hash surface** — and keeps the typed `SealPrefixDivergence`
refusal. It constrains only the shared prefix (the suffix is the new evidence the
seal contributes) and only when a suffix is composed; a strictly-shorter divergent
interior capture is **out of scope**, parked as bead `hm-w1o6` (F3).

**The toys had to become faithful first (F1b / F1c).** The literal invariant is
honest for production and the explorer's `ScriptedMachine` (cursor-bounded
capture, stamp `1 + included == raw.len()`). Two `campaign-runner` toys returned
an **unfaithful** capture read — the whole deterministic play, not the clock-bounded
prefix their own `snapshot` stamps — so only their `sdk_events()` reads needed the
fix, mirroring the frame correction PR #147 already applied:

- `GameToyMachine::sdk_events` now returns `capture().filter(at <= self.vtime)`,
  mirroring its own `snapshot` filter (`at <= vt`). A terminal read (drain/film)
  sits at or past every emission, so it is unchanged; only an interior seal read
  is bounded, keeping `sdk_events().len()` equal to the stamped cut.
- `BoxGuest` (`#[cfg(test)]` fixture) stamped a constant `0` while its rollout
  captured 360 frame-marker firings — itself the C1 under-stamp, admitted only
  through the hole. Its **per-rollout frame drain** (the one a candidate seal
  reconciles) is now clock-bounded and `snapshot` stamps that length. Its
  **setup billboard drain** is deliberately left whole: that drain feeds the base
  seal, which `seal_base` **drops** (never reconciled), and it is read at
  `vtime = base_vtime` where the `len` register (at `base_vtime + 1`) would
  otherwise truncate away and break billboard establishment (`billboard_window_of`
  reads by register, not `Moment`). Cross-surface but blessed (option A, task
  owner); zero production surface, full `campaign-runner` suite green.

**Regression tests (in `campaign::tests`).** The four shipped regressions stay
green under the literal form (asserted `captured` values `{2, 2, 1}` coincide with
`raw.len()` in every case), plus two new for F1/F2:
1. `a_below_baseline_under_stamp_is_refused_loudly` — the C1 repro (stamp 1 vs a
   2-record capture): `SealSuffixDivergence { captured: 2 }`.
2. `a_below_baseline_over_stamp_is_refused_loudly` — stamp 2 vs a truncated
   1-record capture: `SealSuffixDivergence { captured: 1 }`.
3. `an_honest_production_frame_seal_capture_is_accepted` — the honest-host frame
   test stays green (the C1 fix must not reintroduce the V1 false refusal):
   `PASS explorer campaign::tests::an_honest_production_frame_seal_capture_is_accepted`.
4. `a_same_length_prefix_divergent_capture_is_refused_loudly` — equal length,
   equal stamp, one prefix value byte flipped: `SealPrefixDivergence`.
5. **`an_appended_future_moment_record_is_refused_loudly` (F1)** — honest prefix
   plus one record stamped past the cut: `SealSuffixDivergence { captured: 3,
   stamped: 2 }`. The `cut.at`-bounded form ADMITTED this; the literal form
   refuses it.
6. **`a_prefix_id_swap_is_refused_loudly` (F2)** — an equal-length prefix that
   bumps a firing's register id (identical payload + `Moment`, so the commitment
   digest is blind): refused by the structural comparison as
   `SealPrefixDivergence`.

`a_seal_capture_short_of_its_stamped_cut_is_refused_loudly` (the PR #147 divergent
regression) is retained; its `captured` field reports `raw.len()` (`2`) — same
public shape.

**Determinism / hash-neutrality:** honest runs commit no changed hash. Quoted
green: `campaign_replays_bit_identically`, `distinct_seeds_diverge`,
`same_seed_and_config_yield_identical_artifacts`,
`determinism_proptest::branch_run_hash_is_deterministic_and_replay_reproduces_capture`,
`reseed_fold_proptest::draw_carrying_folds_are_bit_identical`.

**Public API:** new `MachineError::SealPrefixDivergence { baseline, expected,
got }` variant → `tests/public-api.txt` carries the four snapshot lines (regenerated
on the pinned nightly `nightly-2026-06-16` in the first attempt; the F2 rewrite kept
the fields, so it is unchanged since). `SealSuffixDivergence`'s shape is unchanged.
No dependency changes (`cargo deny` not required). The seal-reconciliation anchors
(`raw_len`, `&Normalized`, `inherited`) are bundled into a private `SealAnchors`
struct to keep `materialize_candidate` within the argument-count lint.

**Superseded first attempt (PR #150 discovery).** The first attempt shipped the
count half as `cut.sdk_events == (# raw records with Moment ≤ cut.at)` and defended
it as necessary for `GameToyMachine`'s "whole-play" capture. The tribunal rejected
this: the toy's own `snapshot` already filters `at <= vt` for its stamp and its
child-restore capture — the unfaithful party was the toy's `sdk_events()` read, not
the ruled invariant (JC1 REJECTED). The bounded form admitted an appended
future-`Moment` record (F1, P1) because the decode slices by position, and its
content half compared only the commitment digest, blind to an id/schema swap (F2,
P2). This section reflects the corrected fix; the `BoxGuest` faithfulness
correction (JC2) was UPHELD and stands.
