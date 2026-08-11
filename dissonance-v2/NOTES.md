# Dissonance v2 LibAFL prototype

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

- M0 verified the live model invocation on 2026-08-10 with
  `codex-cli 0.147.0-alpha.6.5`: `gtimeout 120 codex exec
  --ignore-user-config --ephemeral --skip-git-repo-check -s read-only -C
  <operator-view> -m gpt-5.6-luna -c model_reasoning_effort="low" -c
  service_tier="fast" --output-schema <schema> -o <last-message> - <prompt>`.
  The CLI banner confirmed Luna, read-only, and low effort; `service_tier="fast"`
  was accepted by the service. The final file was
  `{"status":"ok","answer":4}` and passed the toy-schema check. OpenAI's
  structured-output validator requires every property using `enum`/`const` to
  carry an explicit `type`; the initial type-less toy property was rejected with
  `invalid_json_schema`, then corrected and rerun successfully. Campaign schemas
  follow that stricter form.
- M1 follows the 2026-08-10 option-1 ruling recorded in
  `MODEL-IN-THE-LOOP-PLAN.md`: the triage timing A/B runs on the hand-written
  detector arm, while a separate Luna-labeled base plateau supplies M2. Across
  seeds `0x5eed_d400..=0x5eed_d405` at 10,000 executions, null counts were
  `[275, 290, 345, 352, 265, 345]` (upper median 345), scripted counts were
  `[172, 48, 110, 199, 65, 60]` (median 110), and calibrated Luna counts were
  `[165, 48, 110, 199, 65, 60]` (median 110). Luna reached on 6/6 seeds, used 56
  successful calls with no fallbacks, met the predeclared `median <= 80% of
  null` threshold, and every seed reproduced execution count and corpus under
  recorded-label replay. The separate 10,000-execution base run retained four
  Luna-labeled testcases, remained closed at semantic progress 1 with no target,
  and replayed exactly.
- M1 preserved one failed prompt-calibration run before the passing matrix. The
  initial general prompt let Luna Boost terminal crash novelty in four seeds;
  with the scheduler's 256x Boost multiplier, Luna's median was 3,478 versus
  null's 345. The corrected general policy makes Boost scarce, requires
  `Suppress` + `DeadEnd` for `crashed=true`, and asks the triager to compare a
  candidate with the visible retained corpus. It still never names keys, doors,
  or the meaning of mechanical progress. The failed and passing transcripts live
  in ignored campaign output for audit.
- M1's production wrapper is the `triage-agent` workspace binary and
  `triage-agent/schemas/triage-labels.schema.json`. It fixes Luna, low effort,
  fast service, a 120-second timeout, and a 200-call campaign cap. Each call gets
  a symlink-free copy of the operator view and records request, prompt, raw final,
  parsed labels, stdout/stderr, and metadata. Wrapper failures become recorded
  neutral labels so the fast loop continues; no unit test invokes a model.
- M2's production wrapper is `instrumentor-agent` with
  `instrumentor-agent/schemas/instrumentor-decision.schema.json`. It fixes Luna,
  xhigh effort, fast service, and a 1,200-second timeout. The host accepts only a
  pure `AdventureDetector`, rejects generated source with unsafe, process, file,
  network, environment, time/random/thread, panic, include, or unbounded-loop
  surfaces, compiles an ignored standalone crate offline, runs each detector
  twice on every recorded plateau testcase, and then restarts the persisted
  labeled corpus. Generated keys are global coverage bits reduced modulo 64;
  this mapping is part of the public detector interface because low-bit
  collisions otherwise make a semantically distinct detector vacuous.
- M2 used the maximum five instrumentor calls. The first two were preserved
  harness-calibration calls: one detector's high tags collided modulo 64, and one
  emitted only standalone boolean milestones that were already globally seen.
  After freezing the complete generic feature-map contract, calls 3–5 were the
  three independent trials. All 3/3 emitted different bounded implementations of
  room/visible-state conjunctions, compiled on attempt one, passed deterministic
  fixtures, retained five detector-attributed novelties, and reached the target
  at post-restart execution 10,285 under the same seed `0x5eed_d500` and 20,000
  execution allowance. All three remained active with 648 executions since last
  novelty and all three complete no-model reruns reproduced their ten-entry
  corpora and reports exactly. Detector retirement uses the existing phase-3
  threshold of 10,000 executions without novelty; a deterministic unit regression
  covers novelty reset and mechanical retirement.
- M4 pins `tetanes-core` 0.15.0 (MIT OR Apache-2.0) and keeps the commercial
  ROM external through `HARMONY_SMB_ROM`. The recorded ROM SHA-256 is
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`;
  neither ROM bytes nor a ROM copy enter the worktree. TetaNES is configured
  with all-zero startup RAM, no persistent SRAM directory, no run-ahead, and
  no audio. Campaigns also disable video after proving that video-enabled and
  headless execution yield identical complete RAM traces. A fixed title-screen
  bootstrap is snapshotted as gameplay genesis. The M4 real-ROM smoke proves
  same input → identical RAM trace, prefix snapshot restore equivalence,
  video/headless RAM equivalence, and same-seed miniature-corpus reproduction.
  Unit tests instantiate the same properties with a synthetic in-memory NROM;
  no test reads the SMB ROM.
- M4's base fingerprint remains position-only: screen page, a 64-pixel scroll
  bucket, and a coarse player-y bucket. Operator logs expose only frame count
  and changed WRAM indices; the full 2 KiB WRAM is raw evidence. Controller
  holds are total and clamped to `1..=120`. The initial 60-frame calibration
  could not cross the first scroll-retention boundary in one action, while two
  60-frame run-jumps reached bucket 2. The plan names 60 only as an example of
  a bounded hold, so widening the bound to 120 preserved the frozen map and
  allowed a generic single action to enter the ratchet.
- M5 froze its ladder before search: maximum 1-1 scroll bucket, 1-1 flag,
  reach 1-2, then onward. The no-ratchet control is a seeded no-feedback random
  walk over the same append/perturb/truncate mechanics: it retains one current
  input regardless of novelty and has no corpus. Fixed multi-chord samples were
  rejected as a calibration artifact because they granted several fresh
  extension opportunities per target execution. Across seeds
  `0x5eed_d700..=0x5eed_d705` at 500 executions, ratchet maximum buckets were
  `[3, 9, 7, 5, 7, 5]` (upper median 7) versus random-walk
  `[3, 7, 5, 4, 5, 6]` (upper median 5). The ratchet therefore beats the
  no-feedback control and makes real progress, but both plateau before the 1-1
  flag. A deterministic 512-entry save-state prefix cache restores the longest
  known prefix together with its exact base features and observation trace;
  the cached run reproduced the pre-cache corpus and milestone result exactly.
- M6 ran only after the user explicitly authorized sending per-action 2 KiB
  ROM-derived WRAM snapshots, changed indices, controller inputs, labels, and
  campaign statistics to GPT-5.6 Luna; the ROM itself remained local. Two
  preflight launches failed locally before any model call because the release
  helper binaries had not been built. Their fallback records are preserved in
  separate ignored directories; the clean campaign is
  `target/model-campaigns/m6-authorized-final-20260810`.
- The clean M6 run made 39/39 successful low-effort triage calls with no
  fallback. One xhigh detector call generated a bounded whole-WRAM/change-shape
  fingerprint and passed its deterministic fixture. The first xhigh macro
  candidate was mechanically rejected for the forbidden unbounded-loop token
  `while `; the recorded corrective call generated a bounded parameterized
  five-chord jump arc, compiled offline, and passed the final fixture. This used
  three instrumentor invocations, below the five-call campaign cap.
- At 500 executions for seeds `0x5eed_d800..=0x5eed_d805`, frozen M5 had never
  exceeded scroll bucket 9. M6 maxima were base restart
  `[9, 12, 9, 9, 11, 9]`, Luna triage `[11, 9, 11, 9, 9, 14]`, Luna detector
  `[12, 12, 9, 15, 12, 9]`, and full stack `[11, 11, 11, 9, 11, 11]`.
  Therefore five of six full-stack seeds reached the previously unseen bucket
  10-or-beyond within 500 executions, while the frozen baseline's time is
  right-censored beyond 500. Detector and full-stack seed-0 no-model reruns
  reproduced their complete reports exactly.
- M7 deterministically scans the recorded corpus in order for the first retained
  input that reaches a run's maximum scroll bucket, rejects malformed WRAM
  lengths, and writes an action-boundary PNG strip plus a SHA-pinned manifest.
  The headline full-stack seed-0 film contains 16 frames and visibly reaches
  bucket 11. The renderer also retains modes for the first generic progress,
  1-1 flag, 1-2, and onward inputs when those milestones exist.
- M8 replaces `SingleChoiceScheduledMutator` only on the two SMB paths that can
  schedule a generated mutator. The local five-way chooser remembers whether a
  base or generated mutator emitted the candidate and forwards `post_exec` only
  for the generated producer, so retained-offspring metadata and deterministic
  novelty retirement run on the complete `StdMutationalStage` path. A synthetic
  NROM regression drives that production path and proves one non-retained
  generated child reaches the retirement threshold. The remaining
  `SingleChoiceScheduledMutator` site is M5's four-way base-only ratchet; none of
  its inner mutators implements `post_exec`, so the LibAFL 0.15.4 no-op cannot
  suppress accounting there. Generated SMB mutations remained disabled in
  campaign runs until this regression and the M8 gates passed.
- M9 gives SMB its own `TriageScore` with weights Boost=4, Neutral=1, and
  Suppress=0.01; the Phase 2 score and `LoadLabelsStage` remain unchanged. The
  restart path now notifies the existing weighted scheduler after attaching
  initial labels, runs in fixed 500-execution batches, and synchronously labels
  newly retained testcases between batches. Every compact replay event contains
  the real corpus id, the visibility execution count, the complete `SmbInput`,
  and its labels; the per-testcase file shown to the model contains that input
  plus its observations. The redundant request-level joined log was removed;
  each observation retains its existing mechanical `log_line`.
- M9's local-only 1,000-execution preflight used seed `0x5eed_d900`, attached 12
  neutral fixture labels at execution 500, grew the corpus to 61, reached scroll
  bucket 20, and reproduced the complete report under recorded-label replay.
  Evidence is in the ignored directory
  `target/model-campaigns/m9-neutral-preflight-compact-20260810`; compact label
  events keep the live report to 68 KiB while the full model-facing observations
  remain in per-testcase files.
- After explicit standing authorization for Luna evidence (but never ROM bytes),
  M9's live smoke ran the same seed and budget. All 12/12 calls succeeded with no
  fallback; their labels became visible exactly at execution 500 and comprised
  one Boost plus eleven Neutral decisions. The run retained 60 testcases and
  reached bucket 14, below the neutral preflight's bucket 20. This is the valid
  negative A/B allowed by the plan, not a reason to add a hand-authored progress
  term. Recorded-label replay reproduced the complete 70 KiB report exactly.
  Evidence is in the ignored directory
  `target/model-campaigns/m9-luna-20260810`.
- M10 samples the existing SMB observer at every 16-pixel scroll transition,
  the first player-death frame, and each action endpoint. The base fingerprint
  and milestone ladder use the same 16-pixel x granularity. Death is the
  predeclared SMB engine state `$000e == $0b`; retained M9 traces demonstrated
  the old failure by entering `$0b` and then continuing through the automatic
  reset to a live endpoint. The target now stops the action on that first death
  frame, snapshots the terminal bit, and stops campaign replay, film strips, and
  full-frame video there without adding crash retention or another input.
  Generated non-Start chords retain the `1..=120` type bound but draw 75% of
  their holds directly from `2..=12`; Start remains a one-frame press.
- M10's deterministic synthetic-NROM regressions cover multiple intermediate
  16-pixel observations inside one 120-frame action, same-frame terminal-death
  replay, and the seeded short-hold bias. The real-ROM neutral smoke reused the
  M9 synchronous-label path with seed `0x5eed_d900`: 1,000 executions retained
  67 cases, refreshed 23 labels with zero failures, reached 16-pixel bucket 49,
  and reproduced the complete report under recorded-label replay. The winning
  input stopped at the pipe cluster before the first pit, so the required
  first-pit result is **no**. Its terminal-aware H.264 film contains 986 frames
  (16.433 seconds). Evidence is in the ignored directory
  `target/model-campaigns/m10-neutral-20260810`.
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
