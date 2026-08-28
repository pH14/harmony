# Dissonance LibAFL prototype

## Demo

From this directory, run:

```sh
cargo run --bin demo
```

The command is the demonstration through phase 4a. It runs the
null-versus-scripted triage comparison, the phase 3 blind-spot baseline/rescue,
the full phase 4a 2 × 3 adventure matrix, and a generated detector/macro
build-and-restart. It reports target-execution counts rather than wall-clock
durations.

## What is built

- Phase 0: stock LibAFL byte input, in-process toy target, crash objective, and
  an on-disk corpus plus serialized state that resumes after restart.
- Phase 1: typed `DecisionList`, append/perturb/truncate/splice mutators,
  a seeded combination-lock maze executor, `MaxMapFeedback`, and deterministic
  A/B and same-seed corpus tests.
- Phase 2: `InMemoryOnDiskCorpus`, separate deterministic label and log
  sidecars, `LoadLabelsStage`, `TriageScore` with `WeightedScheduler`, a regex
  triager, null-versus-scripted A/B, and recorded-label replay.
- Phase 3: generated-detector trait and facade, lineage-gated `ScopedFeedback`,
  per-detector novelty accounting, deterministic mechanical retirement, an
  exhaustive append-only plateau proof, and a generate→build→restart→resume
  detector install that crosses a real process boundary.
- Phase 4a: a small adventure target supplies the second implementor that forced
  extraction of the `Target` trait. It includes rooms, inventory, a locked door,
  a goal, a hazard, and snapshots. A room-only base map hides the inventory and
  door transitions. Generated detector features retain those transitions; the
  semantic macro emits the coherent fetch-key/open-door route in one mutation.
- Phase 4a runs `{null, scripted}` triage × `{base, generated detectors,
  detectors + macros}` across explicit seeds. Each cell records deterministic
  per-seed execution counts and their median. Tests require the macro arm to beat
  detector-only under both triagers and require macro-enabled same-seed corpora,
  lineage, producer tags, accounting, and execution counts to match exactly.
- Generated mutators expose only a pure `AdventureInput → AdventureInput`
  function. The host facade validates the result, tags retained testcases with
  their producer, accounts offspring and novelty, and retires a macro after a
  fixed number of non-novel offspring. The install harness emits both detector
  and mutator source, builds a separate crate, and restarts the campaign process.

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
- The generated detector is hand-written by the demo instrumentor, as allowed by
  the phase 3 validation plan. The instrumentor refuses to install until it has
  read both `fuzzer_stats` plateau evidence and a label describing the hidden
  inventory distinction.
- Phase 3 checkpoints the seeded RNG, execution count, current testcase, inputs,
  and parent ids, then reconstructs LibAFL feedback history by replaying the
  persisted corpus before continuing. Full `StdState` deserialization in a newly
  linked installer depended on `SerdeAny` constructor registration for generic
  metadata; the explicit checkpoint avoids unsafe manual registration and records
  exactly the campaign state the restart requires.
- The phase 4a triage seam is a `Triager` over a stable JSON `TriageRequest`.
  Null and regex-scripted implementations drive the checked experiment. A
  JSON-over-stdin/stdout `SubprocessTriager` is available for a future model
  process; no model is called by the demo's matrix logic or by tests.
- Every retained phase 4a testcase, including the genesis seed and base-mutator
  offspring, carries `ProducerMetadata`. Generated-mutator names are assigned by
  the host adapter rather than accepted from generated code.
- Macro retirement is based on consecutive emitted offspring that fail to enter
  the corpus. Skipped mutations do not count, novelty resets the counter, and all
  counters use saturating arithmetic so long campaigns cannot panic on overflow.
- `LoadLabelsStage` compares exact sidecar bytes instead of filesystem modification
  times. Modification times are host state; content comparison preserves the plan's
  changed-file behavior while keeping replay independent of wall-clock metadata.
- The `Target` trait was extracted only after the adventure toy existed. Its
  associated action, observation, and snapshot types are the seams demonstrated by
  the two targets; the completed phase 0–3 executor has not been prematurely
  refactored around unvalidated generic machinery.

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
- `SerdeAny` metadata is convenient inside one binary but chafes at the generated
  process boundary: link-time registration of generic map and stage metadata is
  not a stable restart contract. Replaying the small persisted corpus is both
  deterministic and simpler for this prototype.
