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

# tasks/150 — explorer contract clarifications: version-refusal message + Seal-record accessor contract (hm-s6cb, hm-wshf)

Two independent, minimal-diff fixes on the surfaces PR #151 (F1) and PR #147
(V5) left open. No production behavior changes in either — both are
message/documentation drift closures.

## `hm-s6cb` — the version-refusal message no longer misdiagnoses a future version

`LedgerError::UnsupportedVersion`'s message was one static string claiming
every refused `found` predates task 144 ("a pre-144 ... ledger's advanced
seals would reopen with historically truncated cells"). True for `found <
VERSION` (the only case that existed until now); false for a hypothetical
`found > VERSION` (a future build's file) — that file has no pre-144 history
to misdiagnose.

**Fix:** kept `UnsupportedVersion` refusing exactly as loudly and early as
before (same variant, same `found` field, no behavior change to `open`'s
`found != VERSION` check). Split the rationale tail into a two-arm `if *found
< VERSION { .. } else { .. }` expression **inlined directly in the
`#[error(...)]` format arg**: the `found < VERSION` arm keeps the existing
suffix/truncation/task-144 sentence verbatim; the `else` arm (reached whenever
`open`'s `found != VERSION` check routes here and `found` is not less than
`VERSION`, i.e. `found > VERSION`) gets a plain, version-neutral "newer build
than this one understands" sentence with no claim about pre-144 history.
thiserror accepts an arbitrary expression in the format-string position, so
this stays one `#[error(...)]` attribute, no manual `Display` impl. (First
attempt factored this into a private `version_refusal_reason` helper function
— see the PR #153 review fix batch below for why it was inlined instead.)

**Test:** added `ledger::tests::future_version_is_rejected_without_the_pre_144_claim`
(`found: 4`) alongside the existing `found: 1`/`found: 2` cases — asserts the
refusal still fires (`UnsupportedVersion { found: 4 }`) and that its message
contains neither `pre-144` nor `truncated` nor `task 144`, and does contain
`newer`. The existing `version_two_ledger_is_refused_with_the_suffix_reason`
(`found: 2`) is unchanged and still asserts the suffix/truncated/task-144
wording — confirming the `found < VERSION` arm's text survived the split
verbatim.

## `hm-wshf` — the accessor docs now state exactly what they return

`observations_at_cut`/`observations_at` reduce `self.normalized.events` only,
against a doc claiming the result is "true at this evidence's own cut".
Coincidence with the true cut view is keyed by **ledger-ancestor existence**
(`rollout.parent`), not by `parent_cut`: `compose_observations_at` walks
`rollout.parent` to find ancestor records to compose through, so a
`rollout.parent == None` record has nothing to compose and local ≡ composed
exactly, **regardless of `parent_cut`** — a genesis explore stamps
`parent_cut: Some(genesis_cut)` even with `rollout.parent: None`
(`campaign.rs`'s `pick_base`), so `parent_cut` is only the cumulative-position
*base*, never the lineage key. For a `rollout.parent: Some(..)` record the
local reduction omits every *retained* ancestor contribution (not an absolute
inequality — a fully-GC'd ancestor contributes nothing to
`compose_observations_at` either) — concretely a post-144 (`hm-aqf0`) Seal
batch, whose `normalized.events` holds only the run-forward suffix past the
sealed rollout's terminal (empty, and so no accumulated state reported, for a
seal that did not advance past that terminal even when the retained rollout
it seals carries real state), and equally a **branch-child Rollout** (task
132), whose `normalized` "carries only its own suffix" per its own field doc.
(A first-pass fix keyed this on `parent_cut == None` instead — false for the
modal production record, since every production explore, genesis or branch,
stamps `parent_cut: Some(..)`; corrected in the PR #153 verify fix batch
below, V1.)

**Direction taken (per spec, alongside the hm-j7ie ruling, not redesigned):**
re-document + fence, not compose-aware accessors. A single `Evidence` record
cannot compose (that needs ancestor access — `compose_observations_at`'s job).

**Doc fix (final form, post-V1):** rewrote both accessors' doc comments to
state plainly they return the record-LOCAL reduction over `normalized.events`
alone. Coincidence with the true cut view is keyed by `rollout.parent`
(`None` ⟺ no ledger ancestor to compose), not by `parent_cut` (the
cumulative-position base, `Some` on every production record); for
`rollout.parent: Some(..)` records the local reduction omits every *retained*
ancestor contribution — named explicitly as two distinct shapes, a post-144
Seal batch (empty map for a non-advanced seal of a state-bearing rollout) and
a branch-child Rollout (its `normalized` is only its own suffix past the
branch point, task 132) — both directed at `compose_observations_at` for the
true cut view. `observations_at`'s doc additionally states its `included`
parameter is a **local index** (`take(included)`), not cumulative — unlike
`compose_observations_at`'s `included`. The `parent_cut` field doc's own
"(`None` for a genesis-rooted run)" parenthetical was also corrected: a
production genesis-rooted run carries `Some(genesis_cut)`; lineage is
`rollout.parent`'s job, not this field's. No signature change, no behavior
change — `reduce_at_cut`'s call sites are untouched.

**Explicit example (doc-test-shaped, this crate's convention — no crate here
uses literal rustdoc doctests):** added
`evidence::tests::seal_local_reduction_diverges_from_composed_truth`. Builds a
genesis-rooted Rollout (reg 7 accumulates `{5, 7}`, terminal at cumulative
count 2) and a Seal of it that did **not** advance past that terminal
(`parent_cut` exactly at the rollout's terminal cut, its own `normalized.events`
empty) — the textbook "non-advanced seal of a state-bearing rollout" the drift
names. Asserts `seal.observations_at_cut().is_empty()` (the local, misleading-if-
undocumented view) against `compose_observations_at(&led, &seal, seal.cut.sdk_events)`
recovering `Accumulated({5, 7})` (the true view). `observations_at_cut`'s doc
comment points at this test by name.

**Branch-child Rollout divergence (PR #153 review, pr153-A):** the first
attempt's fork test, `compose_excludes_the_parent_event_at_the_fork_count`,
already builds a branch-child Rollout (parent `[5, 7]`, child suffix `[9]`,
`rollout.parent: Some(1)`, `parent_cut` at count 1) but only asserted the
composed side. Added one more assertion in the same test:
`child.observations_at_cut()` reduces to `Accumulated({9})` (the child's own
suffix alone, missing the inherited `5`), against the existing
`compose_observations_at(&led, &child, 2)` assertion of `Accumulated({5, 9})`
— the same local-vs-composed divergence
`seal_local_reduction_diverges_from_composed_truth` shows for a Seal, now
shown for a Rollout, keyed by the child's retained ledger parent
(`rollout.parent`), not by `parent_cut: Some(..)` (every production record's
shape).

**Genesis coincidence witness (PR #153 verify, V1):** added
`genesis_rollout_local_reduction_matches_composed_truth` — a
production-genesis-shaped record (`rollout.parent: None`, but `parent_cut:
Some(..)` with a nonzero base 3, standing in for a restored pre-campaign
setup prefix) asserts `ev.observations_at_cut() ==
compose_observations_at(&led, &ev, ev.cut.sdk_events)` exactly. This is the
positive witness for the coincidence side of the re-keyed contract:
`rollout.parent == None` means nothing to compose, so local ≡ composed even
though `parent_cut` is `Some(..)` — proving the key is `rollout.parent`, not
`parent_cut`. `observations_at_cut`'s doc comment cites all three tests by
name.

**Caller audit (spec-named, both test-only — no production caller exists):**
- `campaign.rs` `restart_rebuilds_canonical_inputs_from_the_ledger` (the
  no-panic restart check) calls `.observations_at_cut()` over **every** batch
  in the ledger (Rollout and Seal alike) and discards the result — the point is
  that recomputation never panics across every batch shape after a restart,
  not that the value is the true cut. **Wants the local reduction as-is.**
  Left unchanged except a comment recording the audit finding and pointing at
  `assert_view_parity` (same file) as the place cut-correctness IS asserted,
  via `compose_observations_at`.
- `retention.rs` `assignment_upsert_dominates_by_strict_quality` calls
  `e2.observations_at_cut()` to independently recompute the cell key
  `cells.key(e2.cut, &e2.observations_at_cut())` for cross-checking against
  `fold_batch`'s own (production) key. `e2` is a `testkit::seal_evidence(...)`
  fixture with `rollout.parent: Some(0)`, but the test's `led` is empty
  (never appended) — `compose_observations_at` walks `rollout.parent`, finds
  no issue-0 batch in `led`, and contributes nothing, exactly like a
  collected/GC'd ancestor — so its record-local reduction **is** the true cut
  view here too (re-keyed per V1: the reason is the missing ledger ancestor,
  not `parent_cut`). **Wants the local reduction as-is** — migrating it to
  `compose_observations_at` would be a no-op given this fixture's shape, so
  left unchanged except a comment recording why.
- Production Seal-arm code (`retention.rs` `fold_batch`'s `EvidenceRole::Seal`
  match arm) and the parity oracle already call `compose_observations_at`, not
  the local accessors — confirmed unchanged, not part of this task's surface.

**Renaming:** not done. The spec allows it (`local_observations_at`, e.g.) but
does not require it, and states the misleading docs are the defect, not the
name. Kept the smaller diff; `cargo public-api` was regenerated on the pinned
nightly (`nightly-2026-06-16`, `cargo test -p explorer --test public_api --
--ignored`) and confirms **zero drift** from `tests/public-api.txt` — expected,
since a doc-comment-only edit does not change the signatures `cargo
public-api` records.

## PR #153 review fix batch (discovery, head a850dcf7 — one batch, three items)

The discovery tribunal (5 seats + Fable 5 judge) returned `REQUEST_CHANGES`
with two P1s and one P2 riding jointly with one of them; everything else in
the PR (both beads' core mechanism, both caller audits, the divergence test,
zero public-API drift) was verified conformant. Fixed as one batch:

- **pr153-A (P1, CONFIRMED, 4-seat convergence).** The `observations_at_cut`
  rustdoc's coincidence claim ("For a Rollout batch ... the local reduction
  and the true cut view coincide") is false for a **branch-child** Rollout —
  `campaign.rs:624-639` builds these with `role: EvidenceRole::Rollout`,
  `parent_cut: Some(..)`, suffix-only `normalized`, exactly the shape this
  crate's own `parent_cut` field doc and the fork test's fixtures already
  demonstrate (parent `[5, 7]` / child suffix `[9]`). The redirect sentence was
  Seal-scoped, so a caller holding a branch-child Rollout — the majority
  record shape in any campaign with branches — was affirmatively told the
  local accessor is the true cut: the exact contract drift hm-wshf exists to
  close, reintroduced for Rollouts. **Fixed at the time** (this discovery
  batch) at the doc choke point: the coincidence claim was restricted to
  genesis-rooted records (`parent_cut == None`); both lineage-bearing shapes
  (post-144 Seal, branch-child Rollout — anything with `parent_cut:
  Some(..)`) were named as suffix-local, directed at
  `compose_observations_at`. `IMPLEMENTATION.md`'s echo (this file) corrected
  the same way. Added the recommended ride-in: one assertion in
  `compose_excludes_the_parent_event_at_the_fork_count` showing
  `child.observations_at_cut()` (`{9}`, local) diverge from the test's
  existing `compose_observations_at` assertion (`{5, 9}`, composed) — the same
  "show it" pattern as the Seal-side test, now for the Rollout side. No
  accessor redesign, no rename, no API drift — stayed inside the
  re-document-not-compose direction the judge confirmed is spec-encoded.
  **Superseded (verify V1, below):** this prescription's own key —
  `parent_cut == None` — was itself wrong (generalized from the test
  fixtures' shape rather than the production constructors); the true key is
  `rollout.parent == None` (ledger-ancestor existence). See "PR #153 verify
  fix batch" below for the corrected contract.

- **pr153-B (P1, CONFIRMED, judge-recomputed) + pr153-C (P2, rides jointly
  with B).** `cargo mutants --no-shuffle --in-diff` found 1 surviving mutant:
  `ledger.rs:172:14: replace < with <=` inside `version_refusal_reason` — the
  `found == VERSION` boundary is never exercised by the `found: 1/2/4`
  regressions (the only input where `<`/`<=` differ), and `version_refusal_reason`
  was a one-use helper (simplicity finding riding the same fix). Resolved both
  as one coherent choice, picking the review's option (ii): **inlined the
  two-arm `if` directly into the `#[error(...)]` format arg and deleted the
  helper function** (~15 LOC net removed) rather than pinning the boundary
  with an admittedly-unreachable assertion. This kills the mutant structurally
  — cargo-mutants mutates ordinary function-body code, not expressions living
  inside a derive-macro attribute's token stream, so the `<` no longer has a
  mutable, separately-testable home — and removes the one-use helper in the
  same stroke. Recomputed: `cargo mutants --no-shuffle --in-diff` against the
  full PR diff (`git diff origin/main...HEAD`) reports **0 missed** (see Gates
  run below).

## PR #153 verify fix batch (verify event, head 743ce43d — one batch, two items)

The verify event (closer + a fresh gate-auditor-v seat, Fable 5 judge)
confirmed pr153-B/C closed and REFUTED the gate-evasion claim against the
inline fix (V2: the empty-in-diff-mutant-set pass is the gate's own designed
contract, and the claimed alternate surviving mutant is an operand rewrite
outside cargo-mutants' operator-swap set — not a regression the inline
introduced). It also found one new P1, explicitly **not attributed to this
worker**: the worker faithfully implemented the discovery record's own A1
prescription, but that prescription mis-keyed the coincidence condition by
generalizing from the test fixtures' shape rather than the production
constructors.

- **V1 (P1, CONFIRMED, closer).** Coincidence between the local reduction and
  the composed truth is keyed by **ledger-ancestor existence**
  (`rollout.parent`), not by `parent_cut`: `compose_observations_at` walks
  `rollout.parent` to find ancestor records (`evidence.rs`'s `while let
  Some(issue) = parent`), so a `rollout.parent: None` record composes
  nothing beyond its own events and local ≡ composed exactly — `parent_cut`
  only shifts the cumulative position each event is stamped at, a shift
  `reduce_at_cut`'s `take(included)` never has to see: `included` is always
  at least the local vector's own length, so `take` always takes the whole
  local vector regardless of `parent_cut`'s base. Production genesis
  explores stamp `parent_cut: Some(self.genesis_cut)` **with
  `rollout.parent: None`** (`campaign.rs`'s `pick_base`, the `None` choice
  arm) — so the discovery fix's "`parent_cut == None` ⟺ coincide" claim is
  false for the modal production record (every genesis explore); no
  production constructor ever stamps `parent_cut: None` at all (only test
  fixtures and legacy pre-132 decodes do).

  **Fix — six choke points, doc-only + one witness assertion:**
  1. `observations_at_cut`'s doc re-keyed to `rollout.parent`; `parent_cut`
     reframed as the cumulative-position base (`Some` on every production
     record, `None` only in fixtures/legacy decodes, behaving as base 0);
     the `rollout.parent: Some(..)` case reworded from an absolute
     inequality to "omits every *retained* ancestor contribution" (a fully
     collected/GC'd ancestor contributes nothing to
     `compose_observations_at` either, so the two can still coincide there).
  2. `observations_at`'s doc re-keyed the same way, plus states `included`
     is a **local index** (`take(included)`), not cumulative — unlike
     `compose_observations_at`'s `included`.
  3. The `parent_cut` field doc's "(`None` for a genesis-rooted run)"
     parenthetical corrected: a production genesis-rooted run carries
     `Some(genesis_cut)`; lineage is `rollout.parent`'s job.
  4. The fork test's comment (`compose_excludes_the_parent_event_at_the_fork_count`)
     re-keyed: the divergence is the child's retained ledger parent
     (`rollout.parent`, walked via `led`), not `parent_cut: Some(..)` (every
     production record's shape).
  5. Both `IMPLEMENTATION.md` echoes corrected (the `hm-wshf` section above
     and the pr153-A bullet, which now cross-references this section).
  6. **Witness (new test):** `genesis_rollout_local_reduction_matches_composed_truth`
     — a production-genesis-shaped record (`rollout.parent: None`,
     `parent_cut: Some(..)` with a nonzero base 3 standing in for a restored
     pre-campaign setup prefix) asserts `ev.observations_at_cut() ==
     compose_observations_at(&led, &ev, ev.cut.sdk_events)` exactly —
     positive proof the key is `rollout.parent`, not `parent_cut`.

  Also corrected in the same pass (not spec-mandated choke points, but the
  same misattribution): the `retention.rs` caller-audit inline comment and
  its `IMPLEMENTATION.md` echo, which had described `e2`'s (`testkit::seal_evidence`)
  local-reduction-is-truth case as "`parent_cut: None`" — `e2` actually
  carries `rollout.parent: Some(0)`; the real reason local ≡ composed there
  is that the test's `led` is empty, so the walk finds no issue-0 batch and
  contributes nothing (a *missing* ancestor, exactly like a GC'd one), not
  an absence of lineage.

- **V3 (P2, ride-along, extracted from a refuted V2 sub-claim's substance).**
  `foreign_version_is_rejected` (the `found: 1` case) asserted only the
  variant shape, leaving the `found < VERSION` arm's message pinned at only
  one of its two reachable inputs (`found: 2`, via the sibling test). Added
  the same suffix/truncated/task-144 message assertion `found: 1`, mirroring
  `version_two_ledger_is_refused_with_the_suffix_reason` — pins the arm
  across its whole reachable domain (1 and 2).

## Gates run

`cargo build -p explorer --all-features`; `cargo nextest run -p explorer
--all-features` (**155 pass, 1 skip** — `genesis_rollout_local_reduction_matches_composed_truth`
is the one net-new test this verify batch adds;
`foreign_version_is_rejected` gained an assertion in place); `cargo clippy
-p explorer --all-features --all-targets -- -D warnings` (exit 0; only
pre-existing root `clippy.toml` `rand::thread_rng`/`rand::rng`/`rand::random`
config diagnostics, unrelated to this change); `cargo fmt -p explorer --
--check` (clean). `cargo deny check` — advisories/bans/licenses/sources all ok
(no dependency change). No `unsafe` added → no Miri obligation.
`cargo mutants --no-shuffle --in-diff` against the full PR diff
(`git diff origin/main...HEAD > pr.diff`, matching the CI `mutants` job's
invocation): `INFO No mutants to filter`, exit 0 — the whole diff remains
doc comments plus test-only code (the V1/V3 batch adds no production-code
mutable surface), so the mutants gate stays at **0 missed** as expected.
**Hash-neutrality:** neither fix touches the evidence/hash path — `hm-s6cb`
only rewrites an error `Display` string (never hashed/persisted), and `hm-wshf`
is docs plus test-only code; no `reduce_at_cut`/`compose_observations_at`
call site changed. Ran anyway as part of the full suite:
`campaign::tests::same_seed_yields_identical_campaign` and
`retention::tests::same_seed_yields_identical_retention_artifacts` both green.

## Scope fence

Touched only `dissonance/explorer/src/{ledger.rs,evidence.rs,campaign.rs,retention.rs}`
and this file. No other bead, no redesign of the Seal representation
(hm-j7ie's ruling stands), no compose-aware accessor rewrite.

---

# tasks/152 — route advanced-span occurrence/assertion events through fold_batch's verdict folds (hm-mmkf, PR #147 F4)

## The bug (fold-side, capture already done)

Since tasks/144 (hm-aqf0) a candidate seal that ran forward past its rollout's
terminal durably captures the **advanced span** `[rollout_terminal, seal_cut)`
in the Seal batch's own `normalized.events` (the run-forward suffix), and
tasks/146/150 hardened that capture. But `RetentionViews::fold_batch` ran the
`OccurrenceOracle` and the finalized absence fold **only in the Rollout arm**
(`retention.rs`, ex-`409-417`). A seal's suffix was folded for its **cell
assignment** (via `compose_observations_at`) but never **judged**: a
`sometimes`/`reachable` hit or an `always`/`unreachable`/terminal assertion that
existed ONLY in the advanced span was durable yet invisible to the verdict
plane — a **false absence** (a must-hit reported never-satisfied though it was
satisfied in the advanced span), or an uncounted occurrence counterexample.
PR #147's F4 framed the fix as **pure fold-side**; this change is exactly that.

## The fix

Extracted the Rollout arm's verdict machinery into one private
`RetentionViews::fold_verdicts(ev, out)` — `OccurrenceOracle::new().judge(ev)`
with campaign-wide fingerprint dedup into `seen_counterexamples`
(`finalized.counterexamples` + `FoldOutcome.new_counterexamples`), then
`self.absences.observe(ev)` — and call it from **both** arms. The Seal arm now
judges its own `normalized.events` (the advanced span) through the identical
oracle and absence fold the Rollout arm uses:

- **Same oracle/keying, no new scheme.** Dedup is the shared campaign-wide
  `seen_counterexamples` set, so an advanced-span hit counts toward the **same**
  occurrence identity as a rollout hit (a rollout is always folded before its
  seal — lower issue in both the live step and `rebuild`'s `(issue, batch)`
  order — so the fingerprint is already seen and never double-counted).
- **Non-advanced seal contributes nothing.** Its suffix is empty and it
  terminates `Quiescent`, so `judge` finds nothing and `observe` only
  re-registers already-declared must-hits (idempotent — `decode_child_suffix`
  keeps the catalog, so the seal's schema equals the rollout's).
- **Live == rebuild by construction.** Both paths call the one shared
  `fold_batch`; the persisted verdict state (`seen_counterexamples`,
  `finalized.counterexamples`, `absences`) is set-/count-based and
  order-independent, so `canonical_bytes` stays bit-identical (the
  `rebuild_from_checkpoint_matches_live_state` contract holds unchanged).

## Why the tests live at the fold surface (not the campaign marker-clamp)

The verdict folds read `ev.normalized.events` and `ev.terminal` **directly**
(never the cut), so the advanced-span shape is exercised most precisely by
constructing rollout+seal `CompletedRunEvidence` at the fold surface — exactly
how `occurrence.rs` builds oracle inputs. This is also the **only** feasible
surface: the portable toy campaign harness (`testkit::ScriptedMachine`) emits a
**v2** catalog (which declares an expectation but **no verb**, so a firing
decodes with `AssertType == None` and neither `satisfies_must_hit` nor the
occurrence oracle's `Always`/`Unreachable` arms fire — a pre-existing v2 gap,
out of scope here), emits **only state** events, and its seals always terminate
`Quiescent`. So an advanced-span occurrence/assertion firing is unreachable
through the toy machine; the fold-level tests use **v1** catalogs (verb carried)
to drive the real absence/occurrence machinery, reproducing the task-144
structure — a Seal batch whose suffix carries an event the rollout terminal
never captured.

Three gates in `retention.rs` `#[cfg(test)]`:

1. **`advanced_span_sometimes_hit_closes_the_false_absence` (red-before).** A
   `sometimes` must-hit declared by the rollout and never fired (absence
   present, satisfied 0), then fired ONLY in the seal's advanced-span suffix.
   After the fix the absence clears and the satisfied count rises to **exactly
   1** (`== 1`, review F3a — the one hash-feeding dimension not covered by
   fingerprint dedup; a double fold counts 2 and this now catches it). **Red-
   before quote** (restricting `fold_verdicts` to the Rollout arm — the pre-fix
   behavior; post-F4 the call is hoisted, so this is how the pre-fix state is
   reproduced):
   ```
   FAIL explorer retention::tests::advanced_span_sometimes_hit_closes_the_false_absence
   panicked at dissonance/explorer/src/retention.rs:1477:
     the advanced-span sometimes-hit closes the false absence
   ```
   Restoring the unconditional call → green.
2. **`non_advanced_seal_leaves_verdicts_identical`.** With a rollout that
   satisfies one must-hit (local 5) and leaves another standing absent
   (local 6), folding a non-advanced (empty-suffix) seal leaves the absence
   ledger, the `seen_counterexamples` set, and `finalized.counterexamples`
   **byte-identical** — only `finalized.seals` moves (proof the seal was
   genuinely folded). This is the hash-neutrality witness on
   no-advanced-span-event workloads.
3. **`rollout_body_counterexample_is_counted_once`.** An `always`-violation in
   the rollout body is counted once; folding a seal whose suffix does not
   re-carry it reports nothing new, and a second seal whose suffix **re-fires**
   the same property is fingerprint-deduped to the same identity — the count
   holds at one across any seal batch (the "same oracle keying" requirement,
   directly exercised, the non-vacuous heart of this gate). The structural check
   that the seal carries no rollout-body event is **fixture-level** (the seal is
   built with an empty suffix); the PRODUCTION guarantee that a real seal's
   suffix excludes the rollout body is owned by the `decode_child_suffix`
   capture-slicing suite in `campaign.rs`, not this fold-level gate (review F3d
   — earlier wording overstated this as "proven, not asserted").

## Judgment call — surfacing seal-fold counterexamples in `StepReport`

I prototyped extending the per-step report (campaign.rs) with the seal fold's
new counterexamples for symmetry with the rollout fold, then reverted it: it is
outside F4's "pure fold-side" framing, the authoritative checkpointed verdict
state is already complete and tested, and the line is **unreachable by the
portable harness** (v2/state-only/Quiescent-seal, above) → an untestable line
and a guaranteed `cargo mutants --in-diff` miss. The discovery tribunal **upheld
this call** at the merge bar (`StepReport.counterexamples` has zero consumers
outside explorer's own tests; the fix is not inert without it), and parked the
real surface change as bead **hm-5mx0** (needs a v1-verb test machine to become
testable). Ride-along F2-doc applied: `StepReport.counterexamples`'s field doc
now states it carries the **rollout fold only** today, that a seal-found
counterexample lands in the authoritative views (`finalized.counterexamples`,
the dedup set) but not this field, and points at hm-5mx0.

## Review response — PR #155 discovery (REQUEST_CHANGES): F1 version boundary + ride-alongs

The discovery tribunal upheld the core (keying sound, red-before genuine,
judgment call upheld, mutants clean) and returned one **P1** plus ride-alongs.
Applied in this batch:

- **F1 (P1) — persisted-checkpoint version boundary.** The fold-semantics change
  reaches the durable `RetentionCheckpoint`: its verdict views now include
  advanced-span contributions, but a checkpoint written by the pre-PR build
  carries no marker, and `RetentionViews::rebuild` clones a covering checkpoint
  verbatim and re-folds only batches **above** its frontier — so a pre-PR **v3**
  ledger whose checkpoint covers an advanced Seal would reopen silently with the
  false absence intact, unrecoverable once GC collects the raw Seal. Fixed per
  the crate's own hm-j7ie precedent: **`ledger.rs` `VERSION` 3 → 4**, with the
  loud `UnsupportedVersion` refusal now naming the fold-semantics checkpoint
  change (keeping the `found < VERSION` vs `found > VERSION` message split from
  PR #153), a new `## Format v4` module-doc section, and the `VERSION`/error
  docs updated. Version-pinning tests updated: `found: 1/2/3` now refused with
  the fold-semantics reason (new `version_three_ledger_is_refused_with_the_fold_semantics_reason`
  pins the exact stale-checkpoint boundary this bump closes), the future-version
  test moved to `found: 5` and asserts the version-neutral message, and
  `fresh_ledger_is_version_four_and_round_trips` confirms this build writes/reads
  4.
- **F3a (P2 ride) — exact absence-count pin.** Gate 1's `satisfied >= 1` → `== 1`.
  Validated by fault injection: a doubled Seal-arm fold now fails Gate 1 at the
  `== 1` assertion (it was suite-green under `>= 1`, the judge's finding).
- **F3b (P3 ride) — fixture Moment honesty.** `evidence_of` stamps a seal's
  advanced-span firings inside `[rollout_terminal=20, seal_cut)` (Moment 25),
  and a rollout's firings in the body (Moment 10), instead of a blanket
  `Moment(10+i)` that put "advanced-span" events below the terminal. Fidelity
  only — the folds read `normalized.events` + terminal, never moments.
- **F3d (P3 ride) — Gate 3 wording.** Dropped "proven, not asserted"; the
  structural check is fixture-level and the production exclusion is attributed to
  the `decode_child_suffix` suite (comment + IMPLEMENTATION.md above).
- **F4 (discretionary) — taken.** The identical `fold_verdicts` call is hoisted
  above the role match (the dispatch selects only counters/assignment, never
  verdict behavior); the hm-mmkf explanatory comment rides with the hoisted call.
- **F5 (discretionary) — not taken.** Consolidating the third `SDKC` v1-catalog
  fixture into `testkit` would touch `occurrence.rs`'s test surface and broaden
  the diff beyond this batch; left for the hm-5mx0 infrastructure work (which
  introduces the shared v1-verb test machine anyway). Noted, not silently
  dropped.
- **hm-5mx0 scope untouched** (no v1-verb machine, no `StepReport` surface change),
  as directed.

## Gates run

`cargo build -p explorer --all-features`; `cargo nextest run -p explorer
--all-features` (**159 pass, 1 skip** — the 3 fold gates + the net-new
`version_three_ledger_is_refused_with_the_fold_semantics_reason`; the skip is the
nightly-only `public_api` snapshot); `cargo clippy -p explorer --all-features
--all-targets -- -D warnings` (exit 0 — only the pre-existing root `clippy.toml`
`rand::*` config diagnostics, unrelated); `cargo fmt -p explorer -- --check`
(clean); `cargo deny check` (advisories/bans/licenses/sources ok — no dependency
change). `cargo nextest run -p campaign-runner --all-features` (**179 pass** —
the integration determinism proptests
`branch_run_hash_is_deterministic_and_replay_reproduces_capture`,
`draw_carrying_folds_are_bit_identical`, and
`same_seed_and_config_yield_identical_artifacts` included). `cargo mutants
--in-diff <working-tree diff> -p explorer --test-tool nextest`: **5 mutants, 5
caught, 0 missed**. No `unsafe` added → no Miri obligation. **No wire-format,
no dependency, no public-API change** (`fold_verdicts` is private; `VERSION` is
private; `fold_batch`/`FoldOutcome`/`StepReport`/`UnsupportedVersion`
signatures unchanged — the `#[ignore]`d `public_api` snapshot needs no refresh).

**Hash-neutrality / determinism (quoted):**
`retention::tests::same_seed_yields_identical_retention_artifacts`,
`campaign::tests::same_seed_yields_identical_campaign`,
`retention::tests::rebuild_from_checkpoint_matches_live_state`,
`retention::tests::bounded_working_set_holds_cap_and_determinism` (proptest),
`retention::tests::absence_view_survives_expiry_and_gc`,
`retention::tests::finalized_counts_are_exact` — all green, plus the full
pre-existing suite unchanged (no hash moved on workloads without advanced-span
occurrence events). The **intended** verdict change on advanced-span workloads
is the false-absence closure of gate 1. The `VERSION` bump is a durable-format
boundary, not a hash change to any batch/view — same-seed artifacts within one
build are byte-identical as before.

## Scope fence

Touched `dissonance/explorer/src/retention.rs` (the fold + its tests),
`dissonance/explorer/src/ledger.rs` (F1 version boundary + version-pinning
tests), a one-line field-doc correction in `dissonance/explorer/src/campaign.rs`
(F2-doc — no code change), and this file. Scope-fenced beads `hm-btht`
(capture-side evidence coverage), `hm-4gaw`, `hm-f82p`, `hm-w1o6`, and **hm-5mx0**
(the parked StepReport/e2e surface) untouched; the Seal representation
(hm-j7ie/hm-aqf0) and the accessor contract (hm-wshf) are unchanged.

# tasks/153 — retention_report recomputability honesty + observations_at
translation qualifier (hm-f82p, hm-0qpm)

## `hm-f82p` — `retention_report` overclaimed recomputability

`retention_report` (`campaign.rs`) labeled **every** retained batch
`Recomputation::FromRetainedEvidence` unconditionally — even when a ledger
ancestor (walked via `rollout.parent`) had been collected. Pre-dated PR #147
for branch-child rollouts; PR #147's suffix-only Seal representation
(`hm-aqf0`) extended the same overclaim to seals, since a seal's
`rollout.parent` is the rollout it seals. A retained batch's own raw evidence
being present is not enough: `compose_observations_at`'s lineage walk still
needs the collected ancestor's raw evidence, so recomputation still requires
materialization replay.

**Fix.** Added `Recomputation::RequiresAncestorReplay` (`retention.rs`) and a
private `DifferentialCampaign::ancestor_collected` helper (`campaign.rs`) that
walks a retained batch's `rollout.parent` chain one issue at a time —
mirroring `compose_observations_at`'s own ancestor walk exactly (same
`role == Rollout && rollout.issue == issue` lookup among retained batches,
same "collected or foreign ⇒ stop" fallthrough) — checking
`ledger.collected()` at each hop. `retention_report`'s retained-batch loop
picks `RequiresAncestorReplay` over `FromRetainedEvidence` when the walk finds
a collected ancestor. Report-labeling only: `collect`, `compose_observations_at`,
and fold behavior are untouched (the compose-across-collected-ancestor
behavior itself stays fenced to the `hm-4gaw`/`hm-btht` family, per the task
spec).

**Regenerated `tests/public-api.txt`** on the pinned nightly
(`nightly-2026-06-16`, `cargo +nightly-2026-06-16 public-api -p explorer`):
one additive line, `pub explorer::Recomputation::RequiresAncestorReplay`.

**Regression test** (`campaign::tests::retention_report_flags_a_collected_ancestor`):
appends a genesis rollout (issue 1), a middle rollout (issue 2,
`parent == Some(1)`), a decoy Seal colliding with the middle rollout's issue
number (`role: Seal`, same `rollout.issue == 2`, `parent == None`), and a
suffix-only-seal test subject (issue 10, `parent == Some(2)`) directly onto a
freshly constructed campaign's ledger — bypassing `step()` entirely, since
`ancestor_collected` and `retention_report` read only `rollout`/`role`/`cut`,
never `normalized`/`env` content. Asserts the fully-retained graph keeps
`FromRetainedEvidence` for both the genesis rollout and the seal, then
collects the genesis rollout (`finalize_evidence` + `collect_batch`, so
coverage is `CoverageRef::Finalized` — no checkpoint needed) and asserts the
middle rollout **and** the two-hops-away seal both flip to
`RequiresAncestorReplay`, while the genesis rollout itself reports
`RawAvailability::Collected` + `Recomputation::RequiresReplay`. The decoy Seal
is what makes the `role == Rollout` conjunct load-bearing in the test, not
redundant: a first fixture draft (a plain two-generation chain, no decoy, no
Seal subject) passed `cargo mutants --in-diff` only by chance — the chain
never needed the walk's second hop, so a `find()` mutant on that hop went
unexercised. The reworked fixture forces a genuine two-hop walk through a
record whose own `role` (Seal) can never spuriously self-match the ancestor
lookup's `role == Rollout` clause, and the decoy proves the role check itself
matters.

## PR #156 discovery fix batch (`ancestor_collected` did not mirror the compose walk)

The discovery tribunal (5 seats + Fable 5 judge) returned `REQUEST_CHANGES`: one
P1 family, one choke point. `hm-0qpm` was clean (no findings, unchanged by this
batch). `ancestor_collected` (`campaign.rs`) documented itself as walking
lineage "exactly as `compose_observations_at` walks it" but did not — four
judge-confirmed members, fixed as one batch:

- **F1a (CONFIRMED, judge repro).** A child forked **past** its parent
  rollout's terminal (task 144 advanced seal: rollout terminal at count 10,
  Seal run-forward to 20, child `parent_cut.sdk_events == 20`) depends on the
  Seal's suffix — `compose_observations_at`'s own `upper > anc.cut.sdk_events`
  Seal pickup (`evidence.rs:446-459`). Collecting that Seal left the child
  mislabeled `FromRetainedEvidence`: the old walk only ever asked "was a
  Rollout with this issue collected," never "does this hop's fork depend on a
  Seal's run-forward suffix, and was *that* collected."
- **F1b (CONFIRMED, judge repro).** `RunId { issue: 7, parent: Some(7) }` is
  accepted by `EvidenceLedger::append` (content-address + budget only, no
  lineage validation — `ledger.rs:513-547`); the old walk never terminated on
  a cyclic `rollout.parent` chain, hanging `retention_report` — a public read
  API — killed at a 30s timeout by the judge.
- **F1c (CONFIRMED, direct CI evidence).** The mutants CI shard was **red** on
  the reviewed head. The prior write-up claimed the timing-out mutant
  (`ancestor_collected`'s second `==` → `!=`) was green by the same precedent
  as this crate's `.cargo/mutants.toml` `timeout_multiplier` comment — wrong: a
  residual exit 3 is a real signal per the hm-y53x decision table, not a
  claimable precedent, and the timing-out mutant was exactly F1b's loop shape,
  not an unrelated equivalent-mutant timeout. **That claim is withdrawn** (see
  the corrected `cargo mutants` result below).
- **F1d (CONFIRMED, from code).** The tombstone match
  (`self.ledger.collected().any(|(_, tomb)| tomb.rollout.issue == issue)`) had
  **no role filter**, while the sibling retained-ancestor lookup required
  `role == Rollout`. A collected Seal tombstone whose issue collided with an
  unresolvable ancestor issue was mistaken for a collected Rollout ancestor —
  the opposite false positive from the original PR's own decoy-Seal test
  (which only exercised the *retained*-side role filter, not the
  *tombstone*-side one).

**Fix — rewrite, not a patch.** `ancestor_collected` moved off
`DifferentialCampaign` onto a new private `AncestryIndex<'a>` (`campaign.rs`),
built once per `retention_report()` call (closing F2's `O(N²)` ride-along —
per-hop linear rescans of `batch_ids()`/`collected()` — for free, since the
rewrite needs random-access lookups anyway): retained Rollout batches indexed
by issue, retained Seal batches indexed by `(sealed rollout issue,
cut.sdk_events)` — `compose_observations_at`'s own two lookup keys — plus the
matching **role-filtered** collected sets for each. `ancestor_collected` now
walks `rollout.parent` one hop at a time against these indices: a missing
retained-Rollout lookup falls back to the role-filtered collected-Rollout set
(F1d); a strict `upper > anc.cut.sdk_events` gate (proved strict, not `>=`, by
a same-cut non-advanced-seal boundary test) performs compose's own Seal-suffix
lookup and treats a tombstoned matching Seal as a collected ancestor (F1a); a
visited-issue set bounds the walk against a cyclic chain, terminating
conservatively (`false`) on a revisit rather than looping forever (F1b).
Ledger-ingest lineage validation itself stays out of this fix's fence — parked
as `hm-wjv1` for the foreman, per the review's fence ruling.

**Five regression tests** (`campaign.rs` test module):
`retention_report_flags_a_collected_advanced_seal_dependency` (F1a: a
collected advanced Seal's suffix flips the forked-past-terminal child to
`RequiresAncestorReplay`),
`retention_report_ignores_a_collected_seal_at_the_no_gap_boundary` (the
`>`-not-`>=` boundary: a same-cut, non-advanced seal's collection is
irrelevant to a child that forked with no gap),
`retention_report_terminates_on_cyclic_lineage` (F1b: a direct self-cycle and
a stronger mutual two-issue cycle both terminate at `FromRetainedEvidence`
instead of hanging), and
`retention_report_ignores_a_collected_decoy_seal_issue_collision` (F1d: a
collected Seal's issue colliding with an unresolvable parent issue stays
`FromRetainedEvidence`, not `RequiresAncestorReplay`). The pre-existing
`retention_report_flags_a_collected_ancestor` (adapted to the new
`synthetic_evidence` fixture helper, hoisted out of the test body so all five
tests share it) is unchanged in intent.

`cargo mutants -p explorer --no-shuffle --in-diff` against the full task diff:
**11 mutants — 9 caught, 2 unviable, 0 missed, 0 timeout.** (The prior head's
1-timeout/1-missed/1-missed state is superseded; see the withdrawn F1c claim
above.)

## PR #156 verify fix batch (F1e — `AncestryIndex::build` didn't resolve a duplicate issue like compose does)

The verify tribunal confirmed the F1a-F1d rewrite closed clean (walk mirrors
compose, mutants genuinely clean, CI green) but found one new P1 the rewrite
itself introduced: `AncestryIndex::build` (`campaign.rs`) resolved a
duplicate `rollout.issue` differently from `compose_observations_at`.
`batch_ids()` iterates `EvidenceBatchId`s in ascending order (`BTreeMap`);
`compose_observations_at`'s own ancestor lookup is a `.find()` over that same
order, so the **first** match (by ascending id) wins. The `build` loop used
plain `rollouts.insert`/`seals.insert`, which — iterating in that same
ascending order — let each subsequent duplicate clobber the map entry, so
the **last** match (the batch with the *larger* id) won instead. Public
`EvidenceLedger::append` accepts content-distinct Rollout (or Seal) batches
sharing one issue (no lineage validation — `hm-wjv1`), so on that shape the
index's pick could disagree with compose's actual pick, and the report's
label could diverge from the true recomputable state.

**Fix (one line each).** `rollouts.entry(b.rollout.issue).or_insert(b)` and
the symmetric `seals.entry((parent, b.cut.sdk_events)).or_insert(b)` — the
first insert for a given key wins, matching `batch_ids()`'s ascending
iteration exactly. The collected sets (`collected_rollouts`,
`collected_seals`) are membership-keyed `BTreeSet`s, not first/last-sensitive
maps, so they needed no change.

**New regression**
(`retention_report_resolves_a_duplicate_issue_like_compose_does`): a
grandparent (issue 100, later collected) and two content-distinct Rollout
batches sharing issue 1 — Dup A resolves further to the grandparent, Dup B
is a dead end (`parent: None`) — give **opposite** final labels depending on
which one the walk resolves to. The test computes its expected label from
whichever duplicate's `EvidenceBatchId` actually sorts first (not a
hardcoded guess), so it fails deterministically under the old last-wins
behavior regardless of which literal id happens to be smaller. Verified by
hand: reverting `or_insert` back to `insert` locally reproduces the failure
(`left: FromRetainedEvidence, right: RequiresAncestorReplay`); restoring the
fix passes again.

`cargo mutants -p explorer --no-shuffle --in-diff` against the full task
diff (unchanged from the discovery-batch count — `entry`/`or_insert`
introduces no new mutable comparison operators): **11 mutants — 9 caught, 2
unviable, 0 missed, 0 timeout.**

## `hm-0qpm` — `observations_at`'s up-to-translation qualifier

Reworded the `observations_at` doc (`evidence.rs`): for a `rollout.parent ==
None` record, coincidence with `compose_observations_at` holds only **up to
the cumulative-position translation** —
`observations_at(k) == compose_observations_at(ledger, self, base + k)`,
where `base` is the record's own `parent_cut` count (0 when `parent_cut` is
`None`). The prior wording ("`None` means nothing is omitted") was true about
*what* is omitted but silent on the argument translation needed for the two
accessors to actually agree — a caller could read it as license to compare
`observations_at(k)` against `compose_observations_at(ledger, self, k)`
directly, which only coincides when `base` is 0 (fixture/legacy pre-132
decodes); a production genesis record's `parent_cut` is always
`Some(genesis_cut)` with a generally nonzero count.

Added the optional witness in
`genesis_rollout_local_reduction_matches_composed_truth` (the fixture already
uses `base = 3`): `ev.observations_at(1) == compose_observations_at(&led,
&ev, base + 1)` and `!= compose_observations_at(&led, &ev, 1)` — the
translated call agrees, the matched-raw-argument call does not. Doc/test-only,
no behavior change.

## Gates run

`cargo build -p explorer --all-features`; `cargo nextest run -p explorer
--all-features` (**165 pass, 1 skip** — the six `ancestor_collected`
regression tests (one adapted, five net-new: four from the discovery fix
batch plus the F1e duplicate-issue regression from the verify fix batch)
plus the strengthened `genesis_rollout_local_reduction_matches_composed_truth`;
the skip is the nightly-only `public_api` snapshot); `cargo clippy -p explorer
--all-features --all-targets -- -D warnings` (exit 0 — only the pre-existing
root `clippy.toml` `rand::*` config diagnostics, unrelated); `cargo fmt -p
explorer -- --check` (clean); `cargo deny check`
(advisories/bans/licenses/sources ok — no dependency change). `cargo
+nightly-2026-06-16 test -p explorer --test public_api -- --ignored` (green,
snapshot matches the regenerated `tests/public-api.txt` — `AncestryIndex`
stays a private type through both fix batches, no further public-API drift).
`cargo mutants -p explorer --no-shuffle --in-diff <task diff>`: **11 mutants
tested — 9 caught, 2 unviable, 0 missed, 0 timeout.** (Supersedes the
discovery head's claimed "1 timeout matching this crate's loop-mutant
precedent" — that claim is withdrawn per the PR #156 discovery ruling: a
residual exit 3 is a real signal, and the timing-out mutant was surfacing
the F1b cyclic-lineage liveness defect, not an equivalent-mutant timeout.)
No `unsafe` added → no Miri obligation. **No dependency change.**

**Hash-neutrality:** no hash-path code touched — `retention_report` is a
read-only projection over the ledger/views (never folded into any hash), and
`observations_at`'s doc/test change touches no production code path at all.
`campaign::tests::same_seed_yields_identical_campaign` and
`retention::tests::same_seed_yields_identical_retention_artifacts` both green,
unchanged.

## Scope fence

Touched `dissonance/explorer/src/retention.rs` (the new `Recomputation`
variant), `dissonance/explorer/src/campaign.rs` (`AncestryIndex` +
`ancestor_collected` + `retention_report`'s report loop + the six regression
tests + the shared `synthetic_evidence` fixture helper),
`dissonance/explorer/src/evidence.rs` (the `observations_at` doc reword + the
witness assertion), `dissonance/explorer/tests/public-api.txt` (regenerated),
and this file. Did not touch `collect`, `compose_observations_at`, ledger
append/lineage validation (parked as `hm-wjv1`), or fold behavior — the
compose-across-collected-ancestor behavior itself stays fenced to the
`hm-4gaw`/`hm-btht` family, per the task spec and the review's fence ruling.
No dependency changes.

# tasks/154 — ledger-ingest lineage validation (hm-wjv1)

`EvidenceLedger::append` previously validated content-address + budget only
(`ledger.rs`), so three durable lineage shapes no producer emits were
representable in a v4 ledger, each a defect every reader had to defend against
individually:

1. **Self/cyclic parent** (`RunId { issue: 7, parent: Some(7) }`, or a mutual
   cycle) — `retention_report` was walk-hardened to terminate (PR #156), but
   `compose_observations_at` (`evidence.rs`) carries **no** visited set and does
   not terminate on a cyclic `rollout.parent` chain.
2. **Duplicate-issue Rollout batches** — content-distinct Rollouts sharing one
   `rollout.issue`. Every ancestor-by-issue reader (`compose_observations_at`'s
   first-match `.find` over ascending `EvidenceBatchId`; the retention rebuild)
   resolves such an issue to whichever sorts first, so collecting that batch
   silently flips which batch each reader resolves to. Label stability across
   collection is unattainable while both are representable.
3. The general class: durable lineage shapes rejection at ingest closes
   structurally instead of every reader defending against them.

## Fix — reject at the one ingest choke point

`EvidenceLedger::validate_lineage` (`ledger.rs`) runs against the batches
**already** in the ledger, before a record is indexed, and is called from both
paths that admit a `CompletedRunEvidence`:

- **`append`** — after the idempotent content-address early-return, before any
  write or budget check.
- **`replay_frames`** (the `open` path) — for every decoded `Evidence` frame,
  before `apply`. A whole, digest-verified frame carrying a malformed shape is
  refused loudly (never truncated as a torn tail, never silently reinterpreted),
  exactly as `append` would have refused it.

Two additive typed errors (`LedgerError::LineageCycle`,
`LedgerError::DuplicateRolloutIssue`) carry the offending batch id(s) and issue.

**Cycle / self-parent.** A visited-issue walk seeded with the record's own
issue steps `rollout.parent` through **retained Rollout** batches (the exact
node set `compose_observations_at` resolves an issue to — a seal's parent is the
rollout it seals). A revisit is a cycle; `issue == parent` is the length-one
self-parent case, caught on the first step. **Invariant (stated in-code):**
every batch already retained was validated the same way, so the retained
lineage is always a DAG — appends (and replay frames) are ordered, so a cycle
can only ever close *through the batch being appended*. Checking the new
record's chain is therefore sufficient; the visited set also guarantees the
walk itself terminates on any hand-crafted stream.

**Duplicate-issue Rollout.** A Rollout whose `rollout.issue` already has a
retained Rollout (a *different* batch id — the `existing != id` guard keeps
byte-identical re-appends idempotent) is refused. **Per-role:** Seals are
exempt (a seal carries its own distinct issue and continues the rollout it
seals), so the rollout+seal pairing is never broken.

## Parent-existence — dangling parents stay LEGAL (surveyed, not guessed)

The spec required surveying the in-tree producers before deciding whether a
`parent: Some(issue)` referencing an issue absent from the ledger is legal.
**The only production appender of `CompletedRunEvidence` into `EvidenceLedger`
is `DifferentialCampaign::step` (`campaign.rs`):** the rollout append
(`self.ledger.append(&evidence)`) and the seal append
(`self.ledger.append(&seal_evidence)`). The revision-coordinator's `append`
calls are to its **own** `LedgerRecord` ledger, not this one. In `step`:

- a rollout's `parent` is the sealed rollout behind the chosen frontier Entry —
  appended in a **prior** step;
- a seal's `parent` is the rollout appended **earlier in the same step**;
- both `issue`s are `p*.proposal.get()`, and the coordinator mints proposals
  from a single monotone counter (`next_proposal`, `coordinator.rs`), so every
  issue is **globally unique and strictly increasing**.

So honest producers always append parents first — **but** an ancestor can be
legitimately **collected** (proven GC behind a covering checkpoint) while
descendants remain, which is exactly the steady state `compose_observations_at`
already handles ("collected or foreign ancestor: compose the retained prefix")
and `ancestor_collected` mirrors. **Decision:** dangling/missing parents stay
legal. Rejecting a missing parent would couple `append` to retention state and
falsely refuse an honest child whose covered ancestor was already collected —
the retention contract forbids that. The cycle walk is **fenced** accordingly:
a parent that resolves to no retained Rollout simply ends the walk (no cycle
through a batch that is not there). All three harm classes close without a
parent-existence rule.

## Version question — no bump (pure narrowing, verified)

This **restricts** what a v4 ledger accepts and changes the meaning of no
existing well-formed record. Per the ledger's own doctrine a pure narrowing
that refuses previously-writable-but-never-produced shapes needs no `VERSION`
bump. Verified the "no honest v4 file contains them" claim: the only producer
(`step`, above) mints unique ascending issues and appends parents first, so it
**cannot** emit a self/cyclic parent or a duplicate-issue Rollout; the PR #156
repros constructed these only via direct `camp.ledger.append(&synthetic_evidence(..))`
test calls, never a producer. No honest producer of any rejected shape exists,
so `VERSION` stays 4 (no read-old/migration question arises — every honest v4
file already passes the new validation). `compose_observations_at`'s
non-termination is closed **structurally** by this ingest invariant (no cyclic
ledger can be constructed or replayed), so its walk and `ancestor_collected`'s
visited-set bound are retained as **defense-in-depth**, not modified.

## Required-regression → test map

| # | Regression | Test (`ledger.rs` unless noted) |
|---|---|---|
| 1 | Self-parent append refused (typed) | `self_parent_append_is_refused` |
| 2 | Mutual-cycle refused at the closing append | `mutual_cycle_append_is_refused_at_the_closing_append` |
| 3 | Duplicate-issue Rollout refused (both ids named); Seal for it still appends | `duplicate_issue_rollout_is_refused_but_a_seal_for_it_appends` |
| 4 | Replay of each rejected shape refuses loudly (hand-framed via the pre-fix writer) | `replay_refuses_each_malformed_lineage_shape` |
| — | Honest lineage still appends + survives reopen (transparency) | `honest_lineage_appends_and_survives_reopen` |
| 5 | Migrate the PR #156 walk-hardening tests | `campaign.rs`: `append_refuses_cyclic_lineage_at_ingest` (was `retention_report_terminates_on_cyclic_lineage`), `append_refuses_a_duplicate_issue_rollout_at_ingest` (was `retention_report_resolves_a_duplicate_issue_like_compose_does`) |

**Migrated tests (regression 5).** The two PR #156 tests built a cyclic /
duplicate-issue ledger through `append` and asserted the downstream *walk*
survived it. That ledger can no longer be constructed via the public API, so
each is re-scoped to assert the **ingest refusal** (typed, both batch ids named
for the duplicate). The `ancestor_collected` visited-set bound and the
`AncestryIndex::build` / `compose_observations_at` ascending-id first-match
tie-break are now **defense-in-depth** — kept and correct, but no malformed
ledger survives ingest to exercise them. `retention_report_ignores_a_collected_decoy_seal_issue_collision`
survives unchanged: its child (issue 2, parent 1) and decoy **Seal** (issue 1)
are both well-formed under the per-role rule (a Seal never trips the
Rollout-uniqueness check; a parent resolving to no retained Rollout ends the
walk). The retention.rs seal-fold tests (Rollout issue 1 + Seals issues 2/3,
distinct) likewise stay green.

## Gates run (all green, macOS)

- `cargo build -p explorer --all-features` — clean.
- `cargo nextest run -p explorer --all-features` — **170 passed, 1 skipped**
  (the `#[ignore]` public-api test).
- `cargo nextest run -p campaign-runner --all-features` — **179 passed, 1
  skipped**, including the hash-neutrality / determinism suites
  `determinism_proptest::branch_run_hash_is_deterministic_and_replay_reproduces_capture`,
  `gamecampaign::tests::campaign_replays_bit_identically`,
  `maze_campaign::same_seed_and_config_yield_identical_artifacts`,
  `reseed_fold_proptest::draw_carrying_folds_are_bit_identical`.
- `cargo clippy -p explorer --all-features --all-targets -- -D warnings` — clean
  (the two `clippy.toml` config notes about `rand::*` disallowed paths are
  pre-existing, unrelated to this diff).
- `cargo fmt -p explorer -- --check` — clean.
- `cargo deny check` — advisories, bans, licenses, sources ok.
- `cargo mutants -p explorer --no-shuffle --in-diff <task diff>` — **14 mutants
  tested: 10 caught, 4 unviable, 0 missed, 0 timeout.**
- `tests/public-api.txt` regenerated on the pinned nightly
  (`nightly-2026-06-16`, `UPDATE_PUBLIC_API=1`): **purely additive** — the two
  new `LedgerError` variants and their fields, nothing removed or renamed.

**Hash-neutrality.** Ingest validation is a pure pre-check that either refuses a
malformed record or proceeds byte-for-byte as before on a well-formed one, so an
honest run feeds the exact same bytes to the ledger and touches no committed
hash — the determinism suites above (unchanged) confirm it. No `unsafe` added →
no Miri obligation. No dependency change.

## Scope fence

Touched only `dissonance/explorer/`: `src/ledger.rs` (the two error variants,
`validate_lineage` + `retained_rollout`, the `append`/`replay_frames` call
sites, the new ingest-validation tests + two test-fixture helpers),
`src/campaign.rs` (the two migrated regression tests only), and
`tests/public-api.txt` (regenerated). `compose_observations_at`,
`ancestor_collected`, `AncestryIndex`, `collect`, `compact`, and fold behavior
are unchanged — the malformed-ledger walk bounds stay as defense-in-depth. No
sibling crate touched; no dependency changes.
