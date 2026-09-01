# LibAFL integration plan

**Status: implementation plan, companion to `docs/DISSONANCE-FROM-SCRATCH.md`.**
Phases 0–4b are complete in the prototype; current work is defined in
`docs/MODEL-IN-THE-LOOP-PLAN.md`. Read the design first, then
`dissonance/CLAUDE.md` before working in `dissonance/`.

Every API claim below was checked against the **libafl 0.15.4** source (the
crates.io release), not against docs or memory. File references are to that
crate.

Vocabulary rule: use LibAFL's own terms only — *testcase, corpus, input,
executor, observer, feedback, scheduler, stage, metadata*. Do not coin new
terms.

## 1. What LibAFL provides (verified)

| What the design needs | Where LibAFL 0.15.4 provides it |
|---|---|
| Inputs that aren't bytes (key presses, decision lists) | `Input` is a plain trait: any `Clone + Serialize + Deserialize + Debug + Hash` type (`inputs/mod.rs:82`). Mutators are defined per input type (`Mutator<I, S>`, `mutators/mod.rs:111`). |
| A custom way to run the target | `Executor` is one method: `run_target(fuzzer, state, mgr, input) -> ExitKind` (`executors/mod.rs:128`). |
| The novelty check | `Feedback::is_interesting(...)` decides whether a run joins the corpus (`feedbacks/mod.rs:81`). Feedbacks compose with and/or logic. `MaxMapFeedback` tracks which map cells a run newly hit (`feedbacks/map.rs`). |
| Sensors on a run | The `Observer` trait, with hooks before and after each run (`observers/mod.rs:40`). |
| The fork tree (which entry came from which) | Every `Testcase` has `parent_id: Option<CorpusId>` built in (`corpus/testcase.rs:58`). |
| Labels on corpus entries that outside processes can read | `Testcase` carries a metadata map, and `InMemoryOnDiskCorpus` writes each testcase's metadata to a JSON sidecar file next to the input file (`corpus/inmemory_ondisk.rs:57`). |
| Scheduling that respects triage labels | `TestcaseScore::compute(state, testcase) -> f64` (`schedulers/testcase_score.rs:19`) plugs into `WeightedScheduler`. AFL's power schedules (EXPLORE, FAST, COE, …) ship in `schedulers/powersched.rs`. |
| Importing inputs an outside process wrote | `SyncFromDiskStage` (`stages/sync.rs:63`) — drop an input file in a directory, the fuzzer picks it up. |
| Stats a human or agent can read | `AflStatsStage` writes AFL++-compatible `fuzzer_stats` and `plot_data` files (`stages/afl_stats.rs`). |

Nothing requires forking LibAFL. Everything custom is an implementation of
an existing trait.

Related crates, noted but not needed before phase 5: `libafl_qemu` and
`libafl_nyx` (hypervisor/snapshot executors — prior art for plugging a VM in
behind the `Executor` trait).

## 2. How the pieces fit together

### Where the code lives

`dissonance/` is a standalone Cargo workspace, deliberately outside the
harmony root workspace.
`libafl` is pinned at 0.15.4 until phase 5. No dependencies on harmony
crates (`consonance/*`, `dissonance/*`) before phase 5. The directory's
`CLAUDE.md` tells agents working there to read only this plan and the design
sketch, and not to read the old `dissonance/` crates — the rebuild must not
inherit the old decomposition by accident.

Modules inside the `searcher` crate, as they land: `input` (per-target input
types and mutators), `executor` (per-target executors), `feedback` (the
generated-code facades), `triage` (label schema and score), `harness` (the
generate → build → restart loop).

### The fuzzer process

A standard LibAFL fuzzer assembled from stock parts, plus three small custom
pieces:

1. **An input type per target.** For example `Vec<ButtonChord>` (a button
   combination plus a hold duration) for the NES target, or a list of typed
   decisions for the maze target. Mutators: append an action, truncate,
   splice two sequences at a point, perturb one element's parameters.
2. **An executor per target.** Wraps the toy, the emulator, or eventually
   the VM. Internally it may cache snapshots keyed by input prefix: since
   the target is deterministic, a snapshot is equivalent to "genesis plus an
   input prefix", so before running an input the executor can resume from
   the longest cached prefix instead of replaying from the start. This is
   purely a speedup inside `run_target`; the fuzzing loop never sees it.
3. **`TriageScore`.** A `TestcaseScore` implementation that reads triage
   labels out of testcase metadata (Boost / Neutral / Suppress becomes a
   multiplier) and combines that with the stock power-schedule factor.
   Plugs into `WeightedScheduler` unchanged.

The corpus is `InMemoryOnDiskCorpus`, so every testcase and its metadata are
plain files on disk. Those files are the interface between the fuzzer and
everything below.

### Adding a target (the plug-in seam)

Every target — maze, adventure toy, NES game, eventually the consonance VM —
answers the same five questions, which becomes one trait plus one action
type: an action vocabulary (typed, total actions with mutation hooks, so the
generic mutator stack works everywhere), `reset` to genesis, `apply` one
action, `observe` (the evidence detectors and triage read), `fingerprint`
(the base coverage feature, coarse on purpose), an `exit_kind` oracle, and
*optional* `snapshot`/`restore` (default: replay from genesis, which
determinism makes merely slow, never wrong). The executor, mutator stack,
and generated-code facades are generic over this trait; plugging in a new
game means implementing it and nothing else.

Sequencing: do **not** design this trait up front. Phase 1 hardcodes the
maze; the trait is extracted in phase 4a when the adventure toy — the
second target — forces the real seams into view. The NES (phase 4b) then
validates the extraction as a third implementor, and consonance (phase 5)
is the fourth.

### Generated code: detectors

The instrumentor LLM emits **Rust source**, not configuration. A generated
file implements one narrow trait:

```rust
/// The only thing generated code implements. Everything else is scaffolding.
pub trait GeneratedDetector {
    /// Map a run's observations to feature keys; each previously-unseen key
    /// is a novelty. Deterministic, pure, no I/O.
    fn features(&self, run: &RunObservations) -> Vec<FeatureKey>;
}
```

Hand-written scaffolding (not generated) adapts any `GeneratedDetector` into
an `Observer` + map-feedback pair.
Keeping the generated surface to one pure function is what makes generation
reliable and review easy: a generated detector can be unit-tested against
recorded `RunObservations` fixtures with no fuzzer running.

**Install = recompile + restart.** The harness writes the generated file
into the crate, runs `cargo build`, restarts the fuzzer, and the fuzzer
resumes from the on-disk corpus. This is how fuzzing already works when you
re-instrument a target: rebuild, then resume from the queue directory. No
hot-swapping, no WASM, no scripting layer in v1. A restart costs seconds.
Revisit only if rebuild time becomes a measured problem.

**Retirement is mechanical.** Each generated detector feeds its own map. A
detector whose map produces no novelties for a fixed number of relevant
executions is dropped at the next rebuild. No LLM and no wall clock is
involved in that decision.

### Generated code: mutators

The same channel can install a `Mutator`: a pure `input + seed → input`
function that splices a coherent, parameterized pattern into a sequence —
"partition the leader while a write is in flight", "jump-arc of length N".
This is where triage hypotheses land when the gap is in the *action* space
rather than the observation space: multi-action patterns that single-action
mutation would only compose by luck.

Everything mirrors detectors: same install-by-rebuild, unit-testable against
fixture inputs (plus a property test that the output stays valid), and
mechanical retirement — tag each testcase with the mutator that produced it
(testcase metadata again), and drop a generated mutator whose offspring stop
producing novelties.

Sequencing: detectors come first (phase 3) because they are easier to
validate; generated mutators join in phase 4, where the A/B measures their
payoff.

### The triage process

At fixed execution counts, the harness stops the current batch, walks the
corpus directory, sends newly retained testcases and their evidence to any
agent harness, and writes labels before resuming:

```rust
struct TriageLabels {
    interest: Interest,          // Boost | Neutral | Suppress → TriageScore
    duplicate_of: Option<u64>,   // semantic dedup
    flags: Vec<Flag>,            // BugSuspect | InvariantNearMiss | DeadEnd
    tags: Vec<String>,           // free text, for the instrumentor
    summary: String,             // one line
    hypotheses: Vec<String>,     // free text, for the instrumentor
}
```

Only `interest` and `duplicate_of` are consumed by machines; the free-text
fields exist to be read by the instrumentor and by humans.

The fuzzer owns the `.metadata` sidecar files. Triage writes separate
`<testcase>.labels.json` files while the fuzzer is stopped; the harness loads
them into testcase metadata before the next fixed execution batch. There is
no concurrent writer, file-watching stage, modification-time scan, or label
arrival race. Record the execution count at which each label is first loaded;
replay loads it at the same count.

The completed phase 2 fixture keeps its deterministic import stage for
regression coverage; do not refactor it. New target paths use the
fixed-boundary harness above.

### The instrumentor process

An agent with file tools pointed at the fuzzer's output directory. It reads
what a human fuzzing operator reads: `fuzzer_stats` and `plot_data` (stock
`AflStatsStage`), plus the corpus directory with its metadata and label
files. Its output is generated code: a detector or mutator. Triage alone
writes labels. If the corpus outgrows comfortable browsing, the fix is a
better stats file or a query script — operator tooling.

### Determinism and testing

- The executors are deterministic by construction. Seed LibAFL's RNG
  explicitly (`StdRand::with_seed`) so mutation schedules replay. Exclude
  time-derived feedbacks and scheduling factors; the phase 1 determinism
  test is the guard.
- Unit tests, no LLM anywhere: mutator properties, executor determinism
  (same input → identical observations, with the snapshot cache on or off),
  label loading at fixed execution counts, generated-code fixtures.
- Integration test: record every label file and generated file during a
  campaign, in order. Replaying the campaign from (seed, recorded files)
  must reproduce the corpus. This is the no-LLM replay property from the
  design sketch, made concrete.
- Model quality is measured in A/B campaigns (see phases), never in CI.

## 3. Prototype phases

Phases 0–4b are completed evidence, not current instructions. Do not rerun or
refactor them unless the current SMB plan requires a regression test. Phase 5
remains a separate future decision.

**Phase 0 — vanilla spike (days).** A stock byte-input fuzzer against a toy
target with a planted crash, built from LibAFL as-shipped, nothing custom.
Purpose: learn the framework's assembly (state, feedbacks, event manager,
stages) on its happy path, and watch corpus persistence and restart-resume
work. Exit: finds the planted crash; corpus survives a restart.

**Phase 1 — our types (week).** Swap in a custom input (a decision list)
and a maze / state-machine target with known deep states. Write the
mutators. Coverage is a small map over a hash of the target's state, fed to
stock `MaxMapFeedback`. Seed the RNG. Exit: reaches deep states measurably
faster than a random-walk baseline, and two campaigns with the same seed
produce identical corpora (written as a test).

**Phase 2 — external steering, no LLM (week).** On-disk corpus,
`TriageLabels`, `LoadLabelsStage`, `TriageScore` + `WeightedScheduler`.
Drive it with a *scripted* triager — a regex over run logs — and A/B against
a null triager. Scripted-first separates "does the plumbing steer the
scheduler" from "is the model any good", and the scripted triager stays
around as a deterministic CI fixture. Exit: labels measurably shift
time-to-target; a campaign replayed from recorded labels reproduces itself.

**Phase 3 — generated detectors (1–2 weeks).** The generate → build →
restart → resume loop, per-detector novelty accounting,
mechanical retirement. Validate with hand-written detectors first (zero
model variance), then a real model. The target gets a deliberate blind
spot: a state distinction the base map cannot see, so the baseline search
provably plateaus. Exit: a model-written detector gets the fuzzer through
the blind spot; the baseline doesn't.

**Phase 4a — the full experiment on a toy game (1 week).** Extend the maze
into a small adventure game we write ourselves: rooms, keys, locked doors,
an inventory, hazards — a few hundred lines, fully in-process. Because we
control the game, we control the blind spots and can compute exact
progress metrics. Generated mutators join generated detectors here. The full
experiment runs on this target: {null, scripted, cheap-LLM triage} × {base,
generated detectors, detectors + generated mutators}. Exit: metric curves per
configuration. This phase carries the science; phase 4b is the showpiece.

**Phase 4b — NES demo (completed).** The deterministic SMB executor uses the
pinned native QuickNES/libretro adapter for per-frame stepping, joypad input,
direct RAM access, and fixed-buffer in-memory save states. Its ROM and core
binary are supplied externally through `HARMONY_SMB_ROM` and
`HARMONY_QUICKNES_CORE`; neither is copied into or committed to the
repository. Current NES work is specified only in
`docs/MODEL-IN-THE-LOOP-PLAN.md`.

**Phase 5 — the real executor (later, a separate decision).** Consonance
slots in behind the same one-method `Executor` trait; fault schedules become
one more input vocabulary. With four phases of experience, decide whether
production dissonance embeds LibAFL or ports its ideas. This is also the
one deliberate moment to consider a libafl version upgrade.

## 4. Risks

- **Generics.** LibAFL's type parameters compound and error messages are
  hard to read. Phase 0 exists to absorb this. Mitigation: copy a
  maintained example fuzzer's type assembly and modify it; don't compose
  from scratch.
- **Label volume.** Label only newly retained testcases at fixed execution
  boundaries, within the predeclared model-call budget. A failed call records
  neutral labels and does not delay later target-execution batches.
- **Restart cadence.** Rebuild-and-restart per installed file is fine for
  occasional instrumentor calls. Labels need no rebuild; the harness loads
  their files before the next execution batch.
- **Hidden nondeterminism in stock stages.** Calibration and any
  time-based scheduling factors may read the wall clock. The prototype
  excludes time-derived feedbacks; the phase 1 determinism test is the
  guard.
- **Version churn.** LibAFL breaks API between minor versions. Pin 0.15.4
  for all phases; upgrade once, deliberately, at phase 5.
