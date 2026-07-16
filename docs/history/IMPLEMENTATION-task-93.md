# IMPLEMENTATION — task 93 (revisit the reproducer-composition model)

Design re-validation task, not a crate. Outputs: a ruling section in
`docs/DISSONANCE.md` ("Ruling (task 93): keep `EnvCodec::compose`"), the required
property test in `dissonance/explorer/tests/replay.rs`
(`compose_rebase_replays_from_genesis`, 256 proptest cases), and this note.
Filed as `docs/history/IMPLEMENTATION-task-93.md` following the `IMPLEMENTATION-quality-d.md`
precedent (a docs-level task has no crate directory of its own).

## The ruling

**Keep `compose`; genesis-only branching is rejected.** `tasks/12-explorer.md` is
unchanged — the shipped model is confirmed, not modified.

## How the evidence was gathered

- **Empirical signal** (the task's "how often are bugs found below non-genesis
  snapshots?"): a throwaway instrumented campaign over the task-12 toy machine —
  50 campaigns × 300 Multiverse steps, default `CoverageStrategy` (explore period
  3), recording each step's branch base via a `Strategy` decorator. Result:
  **9,950/15,000 steps (66.3%) branched below a non-genesis base; 4,665/7,045 raw
  bug discoveries (66.2%) occurred there.** The measurement test was deleted after
  recording (it printed numbers rather than asserting a property); re-derivable in
  ~40 lines from the public `Strategy`/`Explorer` API.
- **Semantics**: reviewed `dissonance/environment/src/envcodec.rs` (production
  `compose`: one-axis `Moment` re-key, Kani-proved injective/overflow-safe,
  fail-closed on `Seeded` inputs / seed-policy mismatch / `StandingFault`s) against
  `dissonance/environment/src/seeded.rs` (sequential PRNG streams — the reason
  seed-serviced decisions are not splice-invariant).

## The one substantive decision

The environment crate's `compose` doc-comments deferred its fail-closed cases "to
task 93". The ruling **promotes that fail-closed scope to the contract** rather
than widening `compose`: the frontier's `Machine::recorded_env` must emit
**tail-complete** deltas (every decision answered since the branch appears as an
override), so a composed reproducer never re-draws the sequential seed stream
across a splice. Alternative considered and rejected for now: re-keying
`SeededEnv` to counter-mode, `Moment`-keyed draws (as the toy machine does) —
sound and compose-friendly, but it changes task 24's blob/PRNG semantics for a
blob-size optimization nothing yet needs; recorded in the ruling as a future
option, not scheduled.

## Known limitations / integrator notes

- The empirical numbers are from the **toy machine**; real-guest campaign
  frequencies will differ, but the structural point (exploit mode ≈ the
  `explore_period` fraction of steps, and bugs land there proportionally) is a
  property of the strategy, not the toy.
- The **tail-complete `recorded_env` contract** binds the frontier R2 adapter
  (vmm-core glue, not yet built). Nothing in the pure-logic crates enforces it at
  runtime — `Explorer::report` composes without a replay check, so a `Machine`
  impl that violates the contract mints a silently-wrong `Bug.env` (compose
  succeeds; the artifact just doesn't reproduce). The frontier task's acceptance
  gate must therefore be **end-to-end on the real machine**: mint a bug below a
  non-genesis base, then `branch(genesis, bug.env)` and require the same stop +
  hash — i.e. this PR's property gate re-instantiated against the production
  codec and real `recorded_env`, not the toy. (The runtime alternative — a
  verify-on-report replay of every minted bug — was considered and rejected as
  the default: it costs a full replay per bug and the end-to-end gate catches the
  same contract violation once, at integration; it remains a reasonable opt-in
  paranoia knob for the frontier if replays turn out cheap.)
- Standing faults remain non-composable (cross-axis: V-time window vs `Moment`
  offset) until a runtime `Moment → VTime` map exists — per the ruling, a bug
  under a standing fault reproduces via its own genesis-rooted env.
- No public API changed anywhere; `dissonance/explorer` gates re-run green
  (build, nextest 47/47, clippy `-D warnings`, fmt, `cargo deny`). The clippy run
  prints pre-existing `clippy.toml` warnings about unreachable `rand` disallow
  paths — workspace-wide, unrelated to this task.
