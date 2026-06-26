# explorer — implementation notes

The Timeline/Multiverse coverage-guided exploration engine (task 12) — **all of
dissonance policy**: the corpus, novelty scoring, per-run decision policy, and the
mutation/scheduling strategy. Pure logic: no `/dev/kvm`, no guest, no socket, no
wall-clock, no host entropy, no sibling-crate dependencies. Builds and passes every
gate on macOS and Linux. No `unsafe`, so no Miri obligation.

## What was built

- The public API exactly as the spec lists it: the data types (`SnapId`, `VTime`,
  `Environment`, `Answer`, `StopConditions`, `StopMask`, `StopReason`, `CovScore`,
  `RunOutcome`, `Bug`), the locally-defined driver/minting seams (`Machine`,
  `MachineFactory`, `EnvCodec`), the policy seam (`Strategy` + `SeedStrategy`,
  `CoverageStrategy`), the `Corpus`, the `Explorer` engine (`new`/`timeline`/
  `multiverse_step`/`explore`), and `MachineError`.
- The two loops. **Timeline** (`Explorer::timeline`) drives one run `run` ⇄
  `run(resolve)`, answering each surfaced `Decision` via `Strategy::choose` and
  accumulating the reproducer `Environment` (`Machine::recorded_env`); at each
  `SnapshotPoint` it captures the snapshot **with the prefix env + coverage as of
  that fork** (not the whole-run env — see the design note below). **Multiverse**
  (`multiverse_step`) picks/mutates an environment, branches, runs one Timeline,
  scores novelty, admits every forked snapshot if novel (issuing `drop_snap` for
  the non-novel and the evicted), and rebases any `Bug` to genesis before
  reporting.
- The in-crate deterministic **toy machine** + **toy codec** (`tests/common`), the
  stand-in for the production R2-socket adapter + `environment` codec. Because the
  engine only ever sees the `Machine`/`EnvCodec` seams (conventions rule 2), every
  property the gates prove is a property of the engine, not of the toy.

### Module layout

`error.rs` (the `MachineError` enum) · `seam.rs` (the `Machine`/`MachineFactory`/
`EnvCodec` traits) · `strategy.rs` (`Strategy` + the two strategies, driven by a
local xorshift64\* PRNG) · `corpus.rs` (the `Corpus` + the `BTreeSet` novelty
index + eviction) · `engine.rs` (the `Explorer`, `RunOutcome`, `Bug`, and the
`sha2` bug fingerprint) · `prng.rs` (the xorshift64\* generator).

## Key design decisions

- **The explorer is schema-blind; mutation lives here, never in the wire.** The
  engine ferries an opaque `Environment { blob_version, bytes }` and only ever
  mints/mutates/composes it through the `EnvCodec` seam. Task 24 owns the blob
  structure; the engine never parses it. This is the AFL lesson and what lets the
  control plane stay fixed independently of the fault catalog.

- **Genesis-complete vs branch-local reproducers, and why `compose` exists.**
  `Machine::recorded_env` returns a *branch-local* env: overrides keyed by
  decision index *since the last branch*. A run branched off a non-genesis corpus
  snapshot therefore yields an env that is ambiguous on its own (a different base
  would mis-key the overrides). On report, the explorer rebases it to genesis with
  `EnvCodec::compose(corpus_base_env, branch_local)`, re-indexing the delta onto
  the end of the genesis-complete base. The corpus stores only genesis-complete
  base envs (they are admitted only from genesis runs), so the compose base is
  always genesis-complete — verified by `tests/replay.rs`.

- **The toy proves the recompose end-to-end.** The toy's seed answer is a pure
  function of the *absolute* decision index, so a decision is answered identically
  whether it is reached from genesis or resumed from a mid-run branch. That is the
  exact property a real backing needs for a branch-local reproducer to recompose
  to a genesis-replayable one, and it is what makes the toy a faithful stand-in
  rather than a rigged one. The production `RecordedEnv` (task 24) already has this
  shape (a seed plus sparse overrides). The toy codec also keeps the base seed
  when mutating a corpus entry (only an override changes), so the single genesis
  seed reproduces both the frozen prefix and the new suffix consistently.

- **A snapshot is paired with its *prefix* reproducer, not the whole run.** A
  `SnapshotPoint` fork is captured together with `recorded_env`/`coverage` taken
  *at that point* (the prefix that produced the snapshot), and that triple is what
  the corpus admits. Admitting the terminal env instead would store overrides for
  decisions taken *after* the fork, which a later `branch(snap, …)` or a `Bug`
  rebased via `compose(base, branch_local)` would mis-key against the snapshot's
  decision-index origin — breaking genesis replay. Pending snapshots are held in a
  `Vec` and *all* admitted/dropped, so a Timeline that forks more than once never
  leaks a backend handle. (`tests/replay.rs::bug_below_a_continued_snapshot_…`
  guards both; it fails if the terminal env is admitted.)

- **Every corpus entry is genesis-complete by induction.** A snapshot forked below
  a *non-genesis* corpus base is captured branch-local to *that* base; before
  admitting it `multiverse_step` rebases it through the base's own
  genesis-complete env (`compose(base_env, prefix)`), exactly as `report` rebases a
  bug — so a nested snapshot's corpus entry is genesis-complete just like a
  first-generation one, and a child mutation or bug found below it keys from the
  right origin. The base env is captured *before* the admit loop, which may evict
  it. (`tests/replay.rs::bug_below_a_nested_snapshot_…` and
  `every_corpus_entry_replays_its_snapshot_from_genesis` guard this; the latter
  drives a real campaign and fails — `base_offset != 0` — if a nested entry is
  admitted branch-local. The toy forks a deeper `SNAP_AT2` point so nested
  snapshots actually occur.)

- **`timeline` drops, never forgets, pending handles.** A direct `timeline` call
  (the engine's public inner loop) that surfaces a `SnapshotPoint` leaves the
  handle in `pending_snapshots`; the next `timeline`/`multiverse_step`
  `drop_snap`s each leftover before reusing the slot, rather than a bare `clear()`
  that would leak the backend handle across repeated or aborted direct runs.

- **No snapshot handle leaks on a partial fork.** At a `SnapshotPoint`, if
  `recorded_env` fails *after* `snapshot` already minted the handle, that handle is
  `drop_snap`'d (best effort, preserving the original error) before the error
  propagates. And `set_corpus_capacity` — which discards the current corpus —
  `drop_snap`s every kept entry's snapshot first (so it is fallible now,
  `Result<(), MachineError>`), never silently forgetting a handle. Both are gated in
  `tests/gc.rs` (each verified to fail without its fix).

- **Two result categories, fail-loud.** A guest-observable outcome is a
  `StopReason`; a transport/backend failure is a `MachineError`. `multiverse_step`
  propagates a `MachineError` (aborting the campaign) and never turns it into a
  `Bug`; only `Crash`/`Assertion` become bugs. `Explorer::new` returns `Err` (never
  panics) if the initial genesis snapshot fails.

- **Determinism by construction.** The novelty index is a `BTreeSet<(edge,
  bucket)>`; eviction breaks ties by `(score, SnapId)`; every strategy draw comes
  from a caller-seeded xorshift64\* PRNG; the bug fingerprint is a `sha2` digest.
  No `HashMap`/`HashSet` reaches an output, no floats, no wall-clock, no unseeded
  RNG. Same `(strategy seed, machine)` ⇒ identical bugs **and** identical admitted
  corpus — gated at ≥256 proptest cases.

- **Library never panics on untrusted input.** `decode` paths in the (test) codec
  are bounds-checked and total; the engine has no `.unwrap()`/`.expect()` outside
  tests. A hostile/mutated blob surfaces as `MachineError::BadEnvironment`.

### Additions beyond the spec's signatures (conventions rule 3)

All are private-helper-equivalent conveniences; none removes/renames/changes a
specified item. `StopReason::{vtime, is_terminal, is_bug}`; `StopMask::{NONE,
ALL}`; `Corpus::{with_capacity, is_empty, novelty, select, base_env, entry,
drain_evicted}` + `Default`; `Explorer::{genesis, corpus, stop_conditions,
set_stop_conditions, set_corpus_capacity, machine_mut}`;
`CoverageStrategy::with_explore_period`; `SeedStrategy::new`/`CoverageStrategy::new`
(the spec showed the strategy structs with private bodies — a constructor is
required to build them). The frozen surface is in `tests/public-api.txt`.

### Dependencies

`thiserror` (errors) and `sha2` (bug fingerprint; the toy also hashes state with
it) — both on the conventions rule-5 whitelist, so no ask-by-comment needed. No
sibling-crate dependency (rule 2).

## Deviations considered and rejected

- **A 4th `EnvCodec` method to slice a genesis env into a branch-local delta.**
  Rejected: it would widen the public seam the spec fixes at three methods. The
  slice is instead an internal detail of `EnvCodec::mutate` (a corpus pick already
  produces a branch-local mutation), so the engine never slices and the production
  codec is free to implement `mutate` however task 24's structure dictates.

- **Adding `StopMask` decision-class bit constants to the library.** Rejected: the
  `StopMask` is interpreted by the `Machine`, not the engine — the engine carries
  it through unparsed. The toy defines its own class bits + `SNAP_BIT` in tests;
  the integrator binds `StopMask` to the real control-proto / `DecisionClass`
  layout. Only `NONE`/`ALL` (campaign-level conveniences) live in the library.

- **Making the toy machine a public module.** Rejected: it is a test fixture, not
  part of the contract. Keeping it in `tests/common` (as pv-net does for its
  oracles) keeps the frozen public surface to exactly the engine.

## Known limitations / integrator notes

- **Frontier (vmm-core), not here (task non-goals):** the R2 socket client that
  implements `Machine` over `control-proto`; the real coverage producer (SDK-event
  hashing / breakpoint coverage — a later coverage task); and the `Environment`
  internal structure + codec (task 24, bound to `EnvCodec`). The toy machine and
  codec exist only to gate-test the engine.

- **Corpus base eviction vs in-flight children.** `multiverse_step` rebases a bug
  found below a corpus snapshot by composing with that snapshot's base env. If the
  base were evicted *before* its child reported, the genesis recompose base is
  gone; the explorer then surfaces the branch-local env as-is (the bug is real and
  still reproduces from that snapshot, just not from genesis). In this single-
  threaded engine a child reports within the same step it branches, so a live base
  is never evicted mid-flight, and the gate sizes the corpus so it never happens.
  A concurrent production driver should pin a base while children are in flight (or
  keep the genesis env keyed independently of corpus capacity).

- **`StopMask` semantics are the backend's.** The engine treats `StopMask`
  opaquely; the toy's interpretation (class bit = `index % NUM_CLASSES`, `SNAP_BIT`
  = `1 << 31`) is a test convention. Bind it to the decision-class taxonomy at
  integration (`docs/DISSONANCE.md`, "keep them in sync").

- **CI wiring left to the integrator (root files are off-limits, rule 1):** add
  `explorer` to the `public-api` job's `-p` list in
  `.github/workflows/quality.yml`. The `tests/public_api.rs` guard +
  `tests/public-api.txt` snapshot are in place and pass on the pinned nightly
  (`nightly-2026-06-16`); the test skips cleanly when the tooling is absent. No
  `miri` entry is needed (no `unsafe`). `Cargo.lock` is regenerated by the
  integrator (this branch touches only `dissonance/explorer/`, matching the task-24
  branch).

## Mutation testing

`cargo mutants --in-diff` (the `mutants` CI gate) is clean: **96 mutants, 0
missed** (82 caught, 14 unviable). The value types and pure functions are pinned by
golden-value unit tests next to the code — the `xorshift64*` golden sequence
(`prng.rs`), the FNV checksum and the exact `CoverageStrategy::choose` byte
(`strategy.rs`), the AFL `bucket` per range + exact `novelty` counts + eviction
victim with a score tie (`corpus.rs`), `StopReason::vtime`/`is_terminal`/`is_bug`
per variant (`lib.rs`), and the golden `sha2` bug `fingerprint` (`engine.rs`). The
`base_snap == genesis` admit-rebase and `stop_conditions` are pinned via the toy in
`tests/engine_pins.rs`. The 14 unviable mutants are pre-existing equivalents plus the
two `choose -> Default::default()` mutants made structurally unviable by **not**
deriving `Default` on `Answer` (an "empty answer" `Answer(Vec::new())` would be
indistinguishable from a derived default — a blind spot, so the derive is omitted).
`evict_over_capacity` compares scores only (ties by admission order) so the `<`/`-`
operators are killable rather than equivalent.

## Gates

`cargo build/nextest/clippy(-D warnings)/fmt -p explorer --all-features`,
`cargo deny check`, and `cargo mutants --in-diff` all pass; 46 tests (incl. a
≥256-case determinism proptest and the per-function mutation pins) + the ignored,
nightly-only public-api guard. Suite runtime ≈ 0.3 s. Task-specific gates:
toy-machine determinism (`tests/determinism.rs`), Timeline replay / OQ10 + genesis
rebase (`tests/replay.rs`), seed-vs-coverage artifact equivalence
(`tests/strategy_equiv.rs`), novelty scoring (`tests/novelty.rs` + `corpus.rs`
unit tests), two error categories (`tests/errors.rs`), corpus GC (`tests/gc.rs`).
The clippy run also surfaces the three *pre-existing* workspace-`clippy.toml`
meta-diagnostics (the `rand::*` disallowed-method paths are unresolvable once
proptest pulls `rand` into the dev dep graph); they cite no code here and do not
fail `-D warnings`.
