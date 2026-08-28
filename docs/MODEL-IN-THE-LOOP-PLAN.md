# Model-in-the-loop plan: current SMB campaign

**Status: Step 3 execution plan, companion to
`docs/DISSONANCE-FROM-SCRATCH.md` and `docs/LIBAFL-PLAN.md`.** M0–M7 are
complete and are evidence, not instructions. Read `dissonance/CLAUDE.md`
before working here and obey its rules.

## Current status

The typed inputs, deterministic replay, labels, generated code, retirement,
SMB executor, baseline, live-model campaign, and films are built. Step 3
starts from the measured M6 plateau and changes only the existing SMB path.
The record of M0–M7 decisions and results (`dissonance/NOTES.md`) lives in git history.

## The model harness

### Which model, at which settings

The agent behind both seams is **GPT-5.6 Luna**, invoked non-interactively
through the codex CLI (`codex exec`, installed at `~/.local/bin/codex`,
verified ≥ 0.147). Two configurations:

| role | model | mode | reasoning effort |
|---|---|---|---|
| triage (per-testcase labels) | `gpt-5.6-luna` | fast | `low` |
| instrumentor (reads stats + labels, writes code) | `gpt-5.6-luna` | fast | `xhigh` |

Invocation template (triage shown; instrumentor differs only in effort,
schema, and timeout):

```sh
gtimeout 120 codex exec \
  --ignore-user-config --ephemeral --skip-git-repo-check \
  -s read-only -C "$OPERATOR_VIEW_DIR" \
  -m gpt-5.6-luna \
  -c model_reasoning_effort="low" \
  -c service_tier="fast" \
  --output-schema "$SCHEMA_FILE" \
  -o "$LAST_MESSAGE_FILE" \
  - < "$PROMPT_FILE"
```

Notes that are load-bearing:

- **`--ignore-user-config` is required.** Pass the model, effort, service
  tier, sandbox, working directory, schema, and timeouts explicitly. The
  invocation was verified in M0; do not redesign or recalibrate it here.
- **Structured output** comes from `--output-schema <file>`: the model's
  final response must conform to a JSON Schema. The last message lands in
  the `-o` file; the wrapper reads that file, never the event stream.
- **Timeouts and budgets are part of the harness**, not politeness: 120 s
  per triage call, 1200 s per instrumentor call, at most **200 triage calls**
  and at most **5 instrumentor invocations** per campaign. Triage runs between
  fixed target-execution batches. A call that times out or fails to validate
  is recorded as a failure with neutral labels, and the next batch continues.

### The wrapper

The existing `triage-agent` and `instrumentor-agent` binaries implement the
JSON-over-stdin/stdout protocol, structured schemas, recording, timeouts, and
neutral fallback. Their tests use a fake `codex`; no unit test invokes a
model. Reuse these binaries and schemas. Do not add another wrapper, runtime,
or protocol in Step 3.

### The operator view (honesty rules)

The model must see what a human fuzzing operator would see, and **nothing
else**. Codex runs with `-C` pointed at a dedicated operator-view directory
containing only:

- the campaign output: `fuzzer_stats`, `plot_data`, the corpus directory
  with metadata and label sidecars;
- a short, freshly written description of the input vocabulary and the
  observation format (what the fields *are*, not what they *mean* for
  progress);
- the prompt file for the current call.

Never `searcher/src` — the target source contains the planted blind spots and
the scripted triager's regex, and reading them would void the experiment.
The sandbox is `read-only`; the wrapper copies/links evidence in, and all
writes flow back through the structured response only.

### Instrumentor output contract

Reuse the existing single-decision schema and host-controlled install path.
For SMB, the legacy `scope_to_lineage` field remains `null`; do not refactor
the completed adventure path in Step 3. The host compiles generated code,
runs it on recorded observations, and rebuilds against the persisted corpus.
M12 adds checks to this path rather than creating another installer.

### Record and replay (the master property, extended)

Every model interaction — request, prompt, raw final message, parsed result,
and per-call metadata (model, effort, service tier, duration, attempt
number) — is recorded under the campaign directory as it happens. The
existing replay property extends unchanged: **a campaign replayed from
(seed, recorded labels, recorded generated files) with no model present must
reproduce the corpus exactly.** This is both the integration test and the
A/B's defense against "it worked once". Model quality is never asserted in
`cargo test` — campaigns run as bin targets; CI stays deterministic.

## Completed evidence — M0–M7

- The live model invocation, wrappers, schemas, recording, neutral fallback,
  generated-code build path, retirement, and no-model replay were validated on
  the adventure target.
- The deterministic SMB executor, position-only base map, snapshots, baseline,
  live-model arms, and milestone films are built. M6 stopped at the plateau
  summarized below; M7 recorded the corresponding films.
- The ROM remains external at
  `/Users/phemberger/workspace/roms/Super Mario Bros. (World).nes`, is supplied
  through `HARMONY_SMB_ROM`, and has SHA-256
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
  Never commit or copy it into the repository.
- M6's model calls were explicitly authorized for raw WRAM observations,
  changed indices, controller inputs, labels, and campaign statistics. Step 3
  confirms authorization before sending any expanded evidence.

The detailed commands, thresholds, failures, results, and output locations
are in `dissonance/NOTES.md` (git history). Do not rerun or refactor M0–M7 except where a
Step 3 change requires a regression test.

## Step 3 — improve SMB search, then scale (M8–M10, M12–M13)

*(Added 2026-08-10 after the M0–M7 post-mortem. Basis: output analysis of
the M6 campaigns, a doubled-budget probe, and one source-verified LibAFL
bug. The findings are restated here in full because the probe's outputs
live outside the repo.)*

### What the post-mortem established

Five findings, in order of severity:

1. **Generated-mutator accounting never ran (verified bug).** libafl 0.15.4's
   `SingleChoiceScheduledMutator` implements `Mutator::post_exec` as a
   no-op (`src/mutators/scheduled.rs` — parameters underscore-unused,
   nothing forwarded to the selected inner mutator). The SMB
   generated-mutator adapter hangs its novelty/retirement accounting on
   `post_exec`, so in every M6 campaign the generated mutator consumed roughly
   a fifth of the mutation slots, produced zero retained offspring, and could
   never retire. The existing regression exercised the adapter directly and
   missed the wrapper path.
2. **The installed detector floods the corpus.** The whole-WRAM hash
   minted 2,300–3,200 "novelties" in hundreds of executions with 60–70 %
   of executions retained in the corpus. At that rate the scheduler spends a
   shrinking share of its executions on testcases with the greatest max x —
   more compute buys mostly noise — and a hash-everything detector never goes
   quiet, so novelty-based retirement cannot catch it.
3. **Triage was static and mis-weighted.** Labels were attached once to
   the initial 39 corpus entries and never refreshed; Boosted entries
   average *lower* scroll buckets than Neutral ones, yet Boost carries the
   toy phases' 256× multiplier. M6 implemented a one-shot preprocessing pass,
   so newly retained testcases never received model judgment.
4. **The generated-mutator contract permits one deterministic child per parent.**
   `mutate(&self, input) -> input` — the design docs' "pure function" —
   has no seed, so re-selecting a parent regenerates
   the identical suffix and jump-timing variants cannot be searched. The
   purity constraint outran its purpose; a recorded seed preserves replay
   just as well.
5. **Observation density is coupled to action length.** Fingerprints and
   observations fire only at hold boundaries (up to 120 frames), so a hold
   that crosses several 64-px buckets — or walks into the pit — registers
   only its endpoint, and death does not terminate the run.

A doubled budget (1,000 executions, detector arm) confirmed the diagnosis:
no progress past bucket 15; the corpus grew 356 → 605 entries.

### Ground rule: generated artifacts only

The mechanical layer stays stock LibAFL plus the four model outputs:
labels, generated detectors, generated mutators, and generated rankings. This step fixes bugs,
tunes constants and granularity, and gives the models more leverage without
adding engine concepts or fallback systems. Everything remains testcase,
corpus, input, executor, observer, feedback, scheduler, stage, and metadata.

Execution order is M8, M9, M10, M12, then M13.

### M8 — fix the generated-mutator accounting path (P1)

First validate the existing uncommitted M0–M7 work, confirm that it contains
no ROM or accidental campaign/model outputs, then make one checkpoint
commit (at most two if the model harness and SMB work split naturally). Do
not reconstruct eight historical commits. Rebase onto current `origin/main`
before writing M8 code; the branch predates the repository restructuring.
File the `post_exec` bug and the M12 design amendment as GitHub issues.

Replace the SMB `SingleChoiceScheduledMutator` use with the same small,
explicit base-versus-generated-mutator choice already used by the adventure
target; it remembers which mutator ran and forwards `post_exec` only when the
generated mutator ran. Do not add a general wrapper framework, another stage,
or a LibAFL upgrade. The regression test must exercise that complete path.
Audit the other `SingleChoiceScheduledMutator` sites in `phase4b.rs` for the
same assumption. The generated mutator stays disabled until this is green.

### M9 — scheduling honesty (fix the constant, refresh labels)

- Cap the SMB Boost multiplier (≤ 4×, target-tunable; enums unchanged) —
  256× was calibrated for toy corpora.
- Run the search in fixed 500-execution batches. Between batches, label newly
  retained testcases within the existing 200-call budget, load the labels,
  then resume. Record the testcase id and execution count at which each label
  becomes visible; replay loads it at the same count. No asynchronous worker,
  file watcher, modification-time scan, or new stage. Leave the completed
  phase 2 `LoadLabelsStage` and its tests unchanged; SMB does not use it.
- Each SMB corpus file shown to the model contains the existing `SmbInput`
  and its observations. Remove the separately joined `log`; its lines already
  exist in the observations.
- The model supplies the priority judgment. Do not add a hand-authored
  progress term in this step; a negative A/B remains a valid result.
- Exit: deterministic tests prove labels become visible at the specified
  execution count, affect the existing scheduler score, and replay exactly.
  One 1,000-execution smoke run exercises the complete SMB path. M13 owns the
  multi-seed efficacy comparison.

### M10 — density and death

- Decouple observation from action length inside the SMB target: the existing
  observer records each 16-pixel x-bucket transition, death, and the action
  endpoint, so a long hold registers the intermediate map cells without a new
  executor or feedback path.
- Refine x-granularity: 64-px → 16-px scroll buckets.
- Death is terminal: end the run at death. The original input replays to the
  same terminal frame; do not create or specially retain another input.
- Bias mutator hold-sampling toward short holds (2–12 frames) so
  release/re-press edges (successive jumps) become reachable; the type's
  bound is unchanged.
- Exit: deterministic tests cover intermediate bucket observations, terminal
  death, and replay. One 1,000-execution smoke run records whether the best
  input gets past the first pit. M13 owns the efficacy comparison.

### M12 — validate generated code and add the seed

The existing validation path lets model code participate without a human in
the review path.

- Generated-mutator contract v2 takes the input and a seed from LibAFL's
  seeded RNG and may emit parameter variants. The two governing design docs
  already contain this amendment; implement it without another abstraction.
- SMB installs generated detectors globally. Keep the legacy lineage field
  `null` and leave completed phase 3 and adventure code unchanged.
- Extend the existing `validate_artifact` path; do not add new types or a new
  module. Declare thresholds and attempt counts before model calls. Existing
  compile/determinism checks gain a bounded-output check and one paired
  500-execution pilot. Install only when the fraction of executions retained
  in the corpus stays below the fixed cap and max x does not regress versus
  the run without generated code. The M6 whole-WRAM hash must fail the pilot;
  retirement remains the backstop.
- The initial instrumentor call uses the M10 smoke evidence. Further calls
  occur only at M13's 5,000- and 20,000-execution checkpoints, and only when
  max x did not increase during the preceding 500-execution batch. Evidence
  contains the highest-max-x testcases, observations ending in death, and a
  film. Keep one generated file per existing invocation and the existing
  retries.
- Exit: the shotgun fixture is rejected and every attempted generated file
  has a recorded result. Zero accepted files is a valid outcome; do not
  weaken checks or extend the predeclared attempt budget to force an install.

### M13 — scale and test the flag hypothesis

- For each arm and each of ≥ 6 seeds, run one single-instance campaign to
  100,000 executions and record checkpoints from that same campaign at 5,000,
  20,000, and 100,000. Arms are cumulative: mechanical-only, + refreshed
  labels, + validated generated code.
- Hypothesis: the full stack reaches **the 1-1 flag** within 100,000
  executions; stretch: reach 1-2. Reaching neither is a valid result after
  the fixed budget. Preserve films for every first-milestone input, a full
  no-model replay of the best run, and one report comparing the M5 and M13
  curves.

## Non-goals for all steps

**No trajectory seeding, ever (ruled 2026-08-10).** The models judge
evidence and write rules — labels, detectors, generated mutators. They never
supply inputs, trajectories, or solutions; a model-played walkthrough
entering the corpus would void the from-scratch discovery claim this program
exists to test. The boundary, stated once: the model may add eyes to the map
(detectors) and moves to the vocabulary (generated mutators — parameterized
competence like "a jump arc", applied blindly by mutation, never level-specific
knowledge like "where the pit is"); the search alone discovers what to do
with them. This closes the design sketch's open question on interactive
trajectory seeding: the answer is no.

No consonance/VM integration, no fault vocabulary, no libafl version bump,
no changes to the old `dissonance/` stack, no reading it either. No model
calls in `cargo test`. No asynchronous triage, lineage-scoped feedback,
special corpus retention, or parallel search in this step. If a step fights
the harness for more than a day (e.g. fast mode has no valid knob, tetanes
determinism doesn't hold), stop and report with evidence rather than working
around it silently.

## Process

Work on this branch (`claude/agent-campaign-iteration-w28bl4`), rebased onto
current `origin/main`, committing per milestone in the existing
`dissonance:` style. File follow-ups as GitHub issues. Before new model calls,
confirm operator authorization for the evidence being sent and the stated
call budget. At completion (or at a hard blocker), use the handoff flow: push
the branch, open a draft PR whose description grounds a reviewer in this
plan, the matrix results, the films, and the replay evidence.
