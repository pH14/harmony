# LibAFL integration plan

This is an exploration plan, a companion to `docs/DISSONANCE-FROM-SCRATCH.md`.
Read that document first — it explains the design this plan implements. This
document verifies that LibAFL actually provides what the design needs, then
lays out the build in phases. The code lives in `dissonance/`; read
`dissonance/CLAUDE.md` before working there.

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
harmony root workspace (the same pattern as `dissonance/differential-lineage`,
itself a separate workspace root nested below it).
`libafl` is pinned at 0.15.4 until phase 5. No dependencies on harmony
crates (`consonance/*`, `differential-lineage`) before phase 5. The directory's
`CLAUDE.md` tells agents working there to read only this plan and the design
sketch, and not to read the old `dissonance/` crates — the rebuild must not
inherit the old decomposition by accident.

Modules inside the `fuzzer` crate, as they land: `input` (per-target input
types and mutators), `executor` (per-target executors), `feedback` (the
lineage-scoping wrapper and the generated-code facades), `triage` (label
schema and score), `harness` (the generate → build → restart loop).

### The fuzzer process

A standard LibAFL fuzzer assembled from stock parts, plus four small custom
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
3. **`ScopedFeedback<F>`.** A wrapper that gates any feedback `F` on
   lineage: it only fires if the current run's parent chain passes through a
   listed `CorpusId`. This is how a generated detector gets restricted to
   one subtree. Roughly fifty lines against public API.
4. **`TriageScore`.** A `TestcaseScore` implementation that reads triage
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
an `Observer` + map-feedback pair, optionally wrapped in `ScopedFeedback`.
Keeping the generated surface to one pure function is what makes generation
reliable and review easy: a generated detector can be unit-tested against
recorded `RunObservations` fixtures with no fuzzer running.

**Install = recompile + restart.** The harness writes the generated file
into the crate, runs `cargo build`, restarts the fuzzer, and the fuzzer
resumes from the on-disk corpus. This is how fuzzing already works when you
re-instrument a target: rebuild, then resume from the queue directory. No
hot-swapping, no WASM, no scripting layer in v1. A restart costs seconds.
Revisit only if instrumentor cadence ever outpaces rebuild time.

**Retirement is mechanical.** Each generated detector feeds its own map. A
detector whose map produces no novelties for N minutes is dropped at the
next rebuild. No LLM is involved in that decision.

### Generated code: mutators (semantic macros)

The same channel can install a `Mutator`: a pure `input → input` function
that splices a coherent, parameterized pattern into a sequence — "partition
the leader while a write is in flight", "jump-arc of length N". This is
where triage hypotheses land when the gap is in the *action* space rather
than the observation space: multi-action patterns that single-action
mutation would only compose by luck.

Everything mirrors detectors: same install-by-rebuild, unit-testable against
fixture inputs (plus a property test that the output stays valid), and
mechanical retirement — tag each testcase with the mutator that produced it
(testcase metadata again), and drop a macro whose offspring stop producing
novelties.

Sequencing: detectors come first (phase 3) because they are easier to
validate; macros join in phase 4, where the A/B measures their payoff.
Macros that keep earning their place accumulate into a per-target library of
legible, reviewable moves that carries across campaigns.

### The triage process

A separate process — any agent harness — that walks the corpus directory,
reads testcases and their evidence, and writes labels:

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

**Single-writer rule.** The fuzzer owns the `.metadata` sidecar files;
triage never writes them. Triage writes its labels to separate
`<testcase>.labels.json` files, and a small custom stage in the fuzzer
(`LoadLabelsStage`, a periodic scan by file modification time) merges
changed label files into testcase metadata. This avoids racing LibAFL's own
persistence. About a hundred lines; the one piece of glue LibAFL doesn't
ship.

### The instrumentor process

An agent with file tools pointed at the fuzzer's output directory. It reads
what a human fuzzing operator reads: `fuzzer_stats` and `plot_data` (stock
`AflStatsStage`), plus the corpus directory with its metadata and label
files. Its outputs are exactly two, described above: generated code
(detectors and macros) and label edits (energy caps expressed as `Suppress`
labels on subtree roots). If the corpus outgrows comfortable browsing, the
fix is a better stats file or a query script — operator tooling.

### Determinism and testing

- The executors are deterministic by construction. Seed LibAFL's RNG
  explicitly (`StdRand::with_seed`) so mutation schedules replay. Exclude
  time-derived feedbacks and scheduling factors; the phase 1 determinism
  test is the guard.
- Unit tests, no LLM anywhere: mutator properties, executor determinism
  (same input → identical observations, with the snapshot cache on or off),
  `ScopedFeedback` scoping, `LoadLabelsStage` merging, generated-code
  fixtures.
- Integration test: record every label file and generated file during a
  campaign, in order. Replaying the campaign from (seed, recorded files)
  must reproduce the corpus. This is the no-LLM replay property from the
  design sketch, made concrete.
- Model quality is measured in A/B campaigns (see phases), never in CI.

## 3. Phases

Each phase adds exactly one new kind of risk, and each has an exit
criterion. No phase before 5 depends on fault injection, consonance, or any
harmony crate.

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
restart → resume loop, `ScopedFeedback`, per-detector novelty accounting,
mechanical retirement. Validate with hand-written detectors first (zero
model variance), then a real model. The target gets a deliberate blind
spot: a state distinction the base map cannot see, so the baseline search
provably plateaus. Exit: a model-written detector gets the fuzzer through
the blind spot; the baseline doesn't.

**Phase 4a — the full experiment on a toy game (1 week).** Extend the maze
into a small adventure game we write ourselves: rooms, keys, locked doors,
an inventory, hazards — a few hundred lines, fully in-process. Because we
control the game, we control the blind spots and can compute exact
progress metrics. Generated macros join generated detectors here. The full
experiment runs on this target: {null, scripted, cheap-LLM triage} ×
{base, generated detectors, detectors + macros}. Exit: metric curves per
configuration. This phase carries the science; phase 4b is the showpiece.

**Phase 4b — NES demo (optional, 1 week, cuttable without losing the
science).** The same experiment on a real game, for demonstration value.
We do not write an emulator: `tetanes-core` (0.15.0, verified against
source) is a headless NES core with `load_rom`, per-frame stepping
(`clock_frame`), joypad input, RAM access, and in-memory save states
(`save_state`/`load_state`) — its `ControlDeck` documents a deterministic
batch mode explicitly. The executor wrapper is a few hundred lines, and
save states make the snapshot prefix cache nearly free. Use an
open-licensed homebrew ROM, not a commercial one: no licensing problem,
and published source means the RAM layout is documented, which makes
detectors and the progress metric easier to write. If this phase fights
us, cut it and let phase 4a's curves stand.

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
- **Metadata races.** Handled by the single-writer rule. Residual risk is
  `LoadLabelsStage` scan cost on large corpora; bounded by scanning file
  modification times.
- **Restart cadence.** Rebuild-and-restart per installed artifact is fine
  at instrumentor cadence (minutes). It would not be fine at triage
  cadence — which is why triage flows through label files and never needs a
  restart.
- **Hidden nondeterminism in stock stages.** Calibration and any
  time-based scheduling factors may read the wall clock. The prototype
  excludes time-derived feedbacks; the phase 1 determinism test is the
  guard.
- **Version churn.** LibAFL breaks API between minor versions. Pin 0.15.4
  for all phases; upgrade once, deliberately, at phase 5.
