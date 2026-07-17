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
