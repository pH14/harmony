# Dissonance v2 LibAFL prototype

## Demo

From this directory, run:

```sh
cargo run --bin demo
```

The command is the final phase 0–3 demonstration. It runs the null-versus-scripted
triage comparison and the phase 3 blind-spot baseline/rescue comparison, reporting
target-execution counts rather than wall-clock durations.

## What is built

- Phase 0: stock LibAFL byte input, in-process toy target, crash objective, and
  an on-disk corpus plus serialized state that resumes after restart.
- Phase 1: typed `DecisionList`, append/perturb/truncate/splice mutators,
  a seeded combination-lock maze executor, `MaxMapFeedback`, and deterministic
  A/B and same-seed corpus tests.
- Phase 2: `InMemoryOnDiskCorpus`, separate deterministic label and log
  sidecars, `LoadLabelsStage`, `TriageScore` with `WeightedScheduler`, a regex
  triager, null-versus-scripted A/B, and recorded-label replay.
- Phase 3: in progress on this branch.

## Decisions

- All time-to-target comparisons use deterministic target-execution counts, not
  wall-clock time. Wall-clock measurements would violate the replay contract and
  make the tests host-dependent.
- The maze executor owns its map observer and updates it through a custom safe
  `Executor`. This avoids global mutable coverage state and `unsafe`.
- Phase 1 compares semantic corpus order (input plus parent id), not raw serialized
  `StdState` bytes. LibAFL state contains unrelated wall-clock fields and metadata
  maps whose byte order is outside the campaign property being tested.
- Phase 3 retirement uses a deterministic number of executions without novelty
  instead of elapsed minutes. It preserves the plan's mechanical-retirement
  behavior without introducing wall-clock state.
- The phase 3 install harness builds generated Rust in an ignored output-directory
  crate and restarts that artifact against the persisted corpus. The generated
  source therefore remains inspectable while running the demo does not dirty the
  checked-out source tree.
- `LoadLabelsStage` compares exact sidecar bytes instead of filesystem modification
  times. Modification times are host state; content comparison preserves the plan's
  changed-file behavior while keeping replay independent of wall-clock metadata.

## LibAFL friction

- LibAFL 0.15.4's generic assembly is exacting: a custom executor must increment
  the state's execution count itself, and observer hooks only run when execution
  goes through the evaluator.
- `StdMutationalStage::new` performs a variable number of children. Bounded
  experiments use a one-iteration stage so reported target-execution counts remain
  meaningful and replayable.
- The repository-level Clippy disallowed-method table emits three configuration
  warnings when the `proptest` development dependency makes `rand` 0.9 reachable;
  the referenced old paths are no longer functions. Clippy still exits successfully
  under `-D warnings`; fixing that root configuration is outside this directory's
  scope.
