# LibAFL integration plan

**Status: exploration plan, companion to `docs/DISSONANCE-FROM-SCRATCH.md`.**
That document sketches the design (a coverage-guided snapshot fuzzer with an
LLM triage + instrumentation loop); this one verifies LibAFL's actual API
surface against it and lays out the build. API claims below were checked
against **libafl 0.15.4** source (crates.io release), not docs or memory —
file references are to that crate.

Vocabulary rule for this plan: LibAFL's own terms only — *testcase, corpus,
input, executor, observer, feedback, scheduler, stage, metadata*. Where the
sketch doc coined anything ("scoped instrumentation"), this plan reduces it to
a named LibAFL component instead.

## 1. Verified surface

What the design needs, and where LibAFL 0.15.4 actually provides it:

| Design need | LibAFL surface (verified) |
|---|---|
| Non-byte inputs (key presses, decision sequences) | `Input` is any `Clone + Serialize + Deserialize + Debug + Hash` type (`inputs/mod.rs:82`). Custom inputs are first-class; mutators are per-input-type (`Mutator<I, S>`, `mutators/mod.rs:111`). |
| Custom executor (emulator, later consonance VM) | `Executor::run_target(fuzzer, state, mgr, input) -> ExitKind` is the whole contract (`executors/mod.rs:128`). One method to implement. |
| Novelty check | `Feedback::is_interesting(...)` (`feedbacks/mod.rs:81`); feedbacks compose via `feedback_or!`-style logic (`FeedbackLogic`). `MaxMapFeedback` tracks per-run novelties and stamps `MapNoveltiesMetadata` onto the testcase (`feedbacks/map.rs`). |
| Sensors | `Observer` trait with pre/post-exec hooks (`observers/mod.rs:40`). |
| Lineage (fork tree) | `Testcase.parent_id: Option<CorpusId>` built in (`corpus/testcase.rs:58`). |
| Labels on corpus entries, readable by external processes | `Testcase.metadata: SerdeAnyMap`; `InMemoryOnDiskCorpus` persists each testcase's metadata as prettified JSON in a `.<testcase>.metadata` sidecar file (`corpus/inmemory_ondisk.rs:57,258`). |
| Triage-driven scheduling | `TestcaseScore::compute(state, testcase) -> f64` (`schedulers/testcase_score.rs:19`) plugs into `WeightedScheduler`; AFLFast power schedules (EXPLORE / EXPLOIT / FAST / COE / LIN / QUAD) ship in `schedulers/powersched.rs`. |
| External processes injecting corpus entries | `SyncFromDiskStage` (`stages/sync.rs:63`) — AFL-style corpus directory syncing. |
| Operator-readable stats | `AflStatsStage` writes AFL++-compatible `fuzzer_stats` and `plot_data` (`stages/afl_stats.rs`). |

Nothing in the design requires forking LibAFL. Everything custom is an
implementation of an existing trait.

Adjacent crates noted but not needed before phase 5: `libafl_qemu`,
`libafl_nyx` (hypervisor/snapshot executors — prior art for the consonance
executor, not dependencies of the prototype).

## 2. Integration design

### Crate layout

One prototype crate, off to the side of the main build graph until it earns
its way in — e.g. `dissonance/libafl-spike/` as its own small workspace.
Internal modules: `input` (per-target input types + mutators), `executor`
(per-target executors), `feedback` (the scoped-feedback combinator +
generated-code facade), `triage` (label schema + score), `harness` (the
codegen/rebuild loop).

### The fuzzer process

A standard LibAFL fuzzer, assembled from stock parts plus four small custom
components:

1. **Input type per target** — e.g. `Vec<ButtonFrame>` for the NES target, a
   decision list for the maze target. Mutators: append, truncate, splice at a
   point, perturb an element. All implement `Mutator<I, S>`.
2. **Executor per target** — wraps the emulator/toy. Internally it may keep a
   **snapshot cache keyed by input prefix** (determinism ⇒ snapshot ≡ genesis
   + prefix): before running an input, resume from the longest cached prefix
   instead of genesis. This is purely a speedup inside `run_target`; the
   fuzzing loop never sees it.
3. **`ScopedFeedback<F>`** — one reusable combinator: wraps any feedback `F`
   and gates it on lineage, i.e. it only fires when the current run's parent
   chain (via `parent_id`) passes through a listed `CorpusId`. This is the
   whole "scoped instrumentation" idea from the sketch doc expressed as ~50
   lines against public API. Blast radius of a bad generated feedback =
   one subtree.
4. **`TriageScore`** — a `TestcaseScore` impl that reads triage labels from
   testcase metadata (Boost/Neutral/Suppress → multiplier) and multiplies the
   stock AFLFast factor. Plugs into `WeightedScheduler` unchanged.

Corpus: `InMemoryOnDiskCorpus`, so every testcase and its metadata are plain
files — the interchange surface for everything below.

### LLM-written code is the instrumentation primitive

The instrumentor LLM emits **Rust source**, not config. Concretely: a
generated file implements one narrow facade trait —

```rust
/// The only thing generated code implements. Everything else is scaffolding.
pub trait GeneratedDetector {
    /// Map a run's observations to feature keys; each previously-unseen key
    /// is a novelty. Deterministic, pure, no I/O.
    fn features(&self, run: &RunObservations) -> Vec<FeatureKey>;
}
```

— and scaffolding (not generated) adapts any `GeneratedDetector` into an
`Observer` + map-style `Feedback` pair, optionally wrapped in
`ScopedFeedback`. Keeping the generated surface to one pure function is what
makes generation reliable and review/testing trivial: generated code can be
unit-tested against recorded `RunObservations` fixtures with no fuzzer
running.

**Install = recompile + restart.** The harness writes the generated file into
the spike crate, runs `cargo build`, restarts the fuzzer, and the fuzzer
resumes from the on-disk corpus — which is exactly how fuzzing already works
(re-instrument the target, resume from the queue directory). No hot-swap, no
WASM, no scripting layer in v1; the corpus and metadata survive on disk and a
restart costs seconds. If instrumentor cadence ever outpaces rebuild time,
that's the point to revisit — not before.

**Retirement is mechanical.** Each generated detector feeds its own coverage
map; a detector whose map has produced no novelties for N minutes is dropped
at the next rebuild. No LLM in that decision.

### The triage process

A separate process — any agent harness — that walks the corpus directory,
reads testcases + existing metadata, and writes labels. Label schema (a
serde struct registered as testcase metadata):

```rust
struct TriageLabels {
    interest: Interest,          // Boost | Neutral | Suppress → TriageScore
    duplicate_of: Option<u64>,   // semantic dedup
    flags: Vec<Flag>,            // BugSuspect | InvariantNearMiss | DeadEnd
    tags: Vec<String>,           // free text, instrumentor-facing
    summary: String,
    hypotheses: Vec<String>,
}
```

Only `interest` and `duplicate_of` are machine-consumed; the rest exist to be
read by the instrumentor and humans.

**Single-writer rule:** the fuzzer owns `.<testcase>.metadata`; triage never
writes it. Triage writes `<testcase>.labels.json` sidecars, and a small custom
stage in the fuzzer (`LoadLabelsStage`, a periodic scan) merges changed
sidecars into in-memory testcase metadata. This avoids racing LibAFL's own
metadata persistence. (~100 lines against public corpus API; the one piece of
glue LibAFL doesn't ship.)

### The instrumentor process

The instrumentor is an agent with file tools pointed at the fuzzer's output
directory, reading exactly what a human fuzzing operator reads:
`fuzzer_stats` and `plot_data` (stock `AflStatsStage`), plus the corpus
directory with its metadata and label sidecars. "Assemble relevant context
from files" is what agent harnesses do natively (grep, read, jq). If the
corpus outgrows what the agent can browse comfortably, the fix is a better
stats file or a query script — operator tooling.

The instrumentor's outputs are exactly two, both already named: a generated
detector file (see above) and label/priority edits (energy caps expressed as
`Suppress` labels on subtree roots). Nothing else.

### Determinism and testing

- The executor is deterministic by construction (toy targets, emulator with
  fixed init). One check item on LibAFL itself: seed its RNG explicitly
  (`StdRand::with_seed`) so mutation schedules replay; verify no other entropy
  sources leak in (calibration stage timing is the likely offender — measure,
  and exclude time-based feedbacks from the prototype).
- Unit tiers, no LLM anywhere: input mutator properties; executor determinism
  (same input ⇒ identical observations, snapshot cache on or off);
  `ScopedFeedback` scoping; `LoadLabelsStage` merge; generated-detector
  fixtures.
- Integration tier: record every triage label and generated file during a
  campaign; a campaign replayed from (seed, recorded labels, recorded
  detectors) reproduces the corpus. This is the no-LLM replay property from
  the sketch doc, made concrete: the "artifact stream" is just files in a
  directory with an ordering log.
- Model quality is measured separately as A/B campaigns (below), never in CI.

## 3. Phases

Each phase has an exit criterion; no phase depends on fault injection or
consonance.

**Phase 0 — vanilla spike (days).** Build a stock bytes fuzzer against a toy
C/Rust target with LibAFL as-shipped. Purpose: pay the generics learning curve
on the framework's happy path, not on our custom stack. Exit: fuzzer runs,
finds the planted crash, corpus persists and resumes across restarts.

**Phase 1 — custom input + executor (week).** Maze / state-machine target
with a known deep state. Custom input type + mutators, in-process executor,
`MaxMapFeedback` over a state-hash observer, RNG seeded. Exit: reaches deep
states measurably faster than random walk; determinism test passes (two runs,
same seed, identical corpus).

**Phase 2 — triage loop (week).** `InMemoryOnDiskCorpus`, `TriageLabels`,
`LoadLabelsStage`, `TriageScore` + `WeightedScheduler`. Drive it first with a
*scripted* triager (regex over run logs — no LLM), A/B against a null triager.
Exit: scripted labels measurably shift scheduling and time-to-target;
label-recording replay works.

**Phase 3 — generated detectors (1–2 weeks).** The codegen harness:
prompt → `GeneratedDetector` file → `cargo build` → restart → resume.
`ScopedFeedback`, per-detector novelty accounting, mechanical retirement.
First with hand-written detectors standing in for generated ones, then with a
real model. Exit: on a target with a deliberate coverage blind spot (a state
distinction the base map can't see), an LLM-generated detector gets the
fuzzer through it and the baseline doesn't.

**Phase 4 — NES target (1–2 weeks).** Key-press input vocabulary, emulator
executor (a Rust NES core), snapshot prefix cache, distance-into-game metric.
The showpiece demo, and the direct test of the LLM thesis: A/B
{null, scripted, cheap-LLM triage} × {base, generated detectors}. Exit:
published metric curves per configuration.

**Phase 5 — real executor (later, separate decision).** Swap in consonance
behind the same `Executor` trait; fault schedules become one more input
vocabulary. Also the decision point for embed-LibAFL-vs-port-ideas in
production dissonance, informed by where phases 0–4 chafed.

## 4. Risks and chafe points

- **Generics.** LibAFL's type parameters compound; phase 0 exists to absorb
  this. Mitigation: copy a maintained example fuzzer's type assembly and
  mutate it, don't compose from scratch.
- **Metadata races.** Handled by the single-writer rule; the residual risk is
  `LoadLabelsStage` scan cost on large corpora (bounded: scan mtimes).
- **Restart cadence.** Rebuild+restart per detector install is fine at
  instrumentor cadence (minutes); it would not be fine at triage cadence —
  which is why triage never requires a restart (labels flow via sidecars).
- **Hidden nondeterminism in LibAFL stages.** Calibration and time-based
  scheduling factors may read wall-clock. Prototype excludes time-derived
  feedbacks; determinism test in phase 1 is the guard.
- **Version churn.** LibAFL moves fast and breaks API between minors. Pin
  0.15.4 for all phases; upgrade once, deliberately, at phase 5.
