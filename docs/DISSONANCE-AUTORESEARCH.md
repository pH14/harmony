# Dissonance autoresearch charter

This charter controls work whose purpose is to improve Dissonance's search speed,
resource efficiency, or generality. It replaces the charter at commit `2d09ea64`;
ledger entries recorded under that version stay in the ledger. The from-scratch
design and LibAFL plan remain background for the model boundary, the target
interface, and established vocabulary; where an old milestone or mechanism
conflicts with this charter, this charter wins.

The program has one concrete target: from a cold Super Mario Bros. gameplay
genesis, with no previously discovered corpus or route and no game knowledge beyond
the declared mechanical observations, find and replay a complete end-to-end run in
less than 45 minutes on the certified benchmark host. This is an engineering target
and a research benchmark, not permission to specialize the searcher for Mario.

Here **autoresearch** means agents improving the mechanical search implementation
between fixed experiments. It does not mean an LLM participating in a campaign.
Model triage, generated detectors, generated mutators, trajectory seeding, and the
previous model-in-the-loop outer design are outside this program. They remain frozen
unless a later charter reintroduces one through the same evaluator after the
mechanical core is fast, bounded, generic, and understood.

## 1. What is being optimized

Wall time to victory factors into two numbers with different measurement needs:

```text
time to victory  =  executions to victory  /  executions per second
```

- **Executions to victory** is a deterministic integer: a pure function of the
  searcher commit and the campaign seed. It is identical on every host. This is the
  search lane's only currency, and the lane runs on any available machine — a
  laptop, a CI runner, a shared box — with no isolation, affinity choreography, or
  host attestation. Two hosts disagreeing on a deterministic counter is a
  determinism bug, never measurement noise.
- **Executions per second** is a hardware property of a compiled champion. The
  systems lane measures it, and that lane runs only on a dedicated benchmark host
  with a fixed governor and no co-tenants. That host is ms02, granted to this
  program exclusively (sections 5 and 13).

The headline clock starts after the release binary receives the ROM and before it
constructs its first target. It stops when the search first records an input that a
separate mechanical verifier replays from genesis to the victory state.
Compilation, fixture creation, and prior campaigns are outside the clock; target
initialization, search, snapshotting, coordination, recording, and victory
verification are inside it.

One lucky run is useful evidence, not a publishable result. Three claim levels:

1. **Breakthrough:** at least one of five predeclared, sealed campaign seeds
   completes within the frozen cold-campaign budget (section 8).
2. **Engineering target:** at least four of those five seeds complete and their
   median executions to verified victory is within the budget.
3. **Transfer claim:** the same compiled searcher and default policy improves the
   withheld suite without target-specific tuning.

The 45-minute wall-time form of these claims is computed only on the certified
benchmark host, as budget executions divided by that host's certified throughput.
The final claim reports every seed, including failures. A full campaign starts from
genesis and may not import an archive, snapshot checkpoint, action sequence,
learned route, or model-generated trajectory.

## 2. The three scorecards

Every experiment belongs primarily to one scorecard. A change may affect the other
two, but it must not hide a regression there.

### 2.1 Search effectiveness — deterministic integers only

Every metric on this scorecard is an integer computed from the recorded campaign
stream, identical across hosts:

- executions and emulated frames to each mechanical progress milestone;
- executions to first verified victory;
- success count across predeclared seeds within the frozen budget;
- area under the best-progress-by-execution curve;
- retained descendants per 1,000 parent selections;
- new archive slots and new observation cells per 1,000 executions;
- longest barren selection streak before new progress;
- executions spent revisiting, replacing, or extending nonproductive states;
- best known input cost to each reached observation cell.

Wall time may be logged as an advisory sidecar. It never enters a search decision,
a kill rule, a ranking, or a promotion. It does govern operations: every run leg
carries a wall-clock deadline projected from its own recorded rate, and a leg that
exceeds its deadline is killed and recorded as a timeout, never waited on.

### 2.2 Systems performance

Frames per second, jobs per second, snapshot and coordinator costs, worker scaling,
and heterogeneous-core efficiency are certified per promoted champion binary on the
dedicated benchmark host, using the lane protocol in section 13. No per-candidate
decision in the search lane depends on any of them.

### 2.3 Architecture and simplicity

Measured at every promotion:

- non-test lines in generic search code and target-specific code;
- public types and traits;
- live policy choices and command-line knobs;
- serialized fields and versioned formats;
- state-affecting hot-path branches;
- dependencies;
- concepts needed to explain the design.

Equivalent performance with less machinery wins. A more complex challenger needs a
material, repeated gain and an explanation of why the mechanism should transfer.

## 3. Non-negotiable boundaries

The intended dependency direction is:

```text
generic searcher  ->  machine interface  <-  target machine implementation
       ^
       |
mechanical observation adapter (one per game or system)
```

The boundaries mean:

- **The searcher is target-blind.** It may know actions, observations, snapshots,
  terminals, deterministic cost, and generic archive relationships. It may not know
  Mario, screens, rooms, pipes, levels, controller buttons, NES memory addresses,
  distributed-system events, database operations, or target-specific policy names.
- **The machine implementation is search-blind.** It implements the machine verbs
  and efficient state transport. It does not decide novelty, quality, admission,
  parent selection, mutation energy, retirement, or progress.
- **The observation adapter is mechanical.** It may define the action vocabulary,
  boot-to-gameplay genesis, observations such as world/level/x/y, and mechanical
  terminal states such as death and victory. It may not choose archive grouping,
  admission filters, selection weights, rollout distributions, retirement rules, or
  search phase changes.
- **The evaluator may understand the benchmark.** Level fixtures, milestone
  scoring, and victory verification live outside the production searcher.
  Evaluation knowledge must never flow back into a run.

The acceptance test is structural as well as behavioral: generic search modules
must not import a target adapter; target modules must not implement search
policies. A new target requires a machine or observation adapter, an action
vocabulary, a mechanical observation schema, terminal predicates, and resource
bounds — never a new scheduler.

Stage 0 of every experiment enforces this mechanically (section 9): a diff audit
rejects game constants — level numbers, coordinates, addresses, tuned thresholds
keyed to a particular target — anywhere above the observation adapter.

## 4. Games are benchmark columns

The benchmark suite is a set of columns, one per game or target system. Columns are
split once per evaluator version into:

- **In-loop columns**, evaluated on every candidate in every round. Super Mario
  Bros. is the first; each new game adds a column when its adapter passes the
  fixture certification in section 8.
- **Withheld columns**, consulted only at batch synthesis when a champion is
  proposed for promotion to `main`. Agents see aggregate pass/fail and normalized
  metrics for a frozen candidate; they may not inspect fixture identities,
  snapshots, traces, or per-case failures.

Candidate ranking within a round aggregates in-loop columns by worst case, never by
average: a candidate's score on the suite is its weakest column. A change that
helps one game by overfitting earns nothing.

Bringing up a new game touches only its own adapter directory, image build, and
evaluator fixtures. Adapter bring-up tasks may run in parallel with search rounds
without coordination.

## 5. Determinism and honesty

The repository's determinism contract applies without exception. In particular:

- same recorded stream plus recorded external artifacts must replay
  byte-identically;
- all state-affecting randomness comes from recorded or derivable seeds;
- no wall clock, floating point, host entropy, unordered iteration, model response,
  or thread timing enters replayable campaign state;
- live worker completion order may choose the recorded stream, but that stream is
  the run identity and must replay exactly;
- every scorecard-2.1 metric must be recomputable from the recorded stream alone;
- benchmark wall time, CPU counters, and process memory are observation-only
  sidecars;
- failed, timed-out, and killed experiments remain in the result ledger;
- no experiment is rerun with a larger budget merely because an agent believes it
  might improve later.

**Cross-host identity smoke test:** relevant only when the fleet has more than
one evaluation host and results must transfer between them: the controller
replays one fixed micro-campaign on two hosts and compares stream hashes and
every deterministic counter, once per serialization or fleet change, never per
candidate and never above micro scale. On a single-host fleet no cross-host
check of any kind runs.

All ranked runs of a round execute on one primary evaluation host, so candidate
comparisons never depend on cross-host behavior. The program has exclusive use of
the ms02 box: no co-tenant workload runs there while a campaign, screen, or
benchmark is live, and no campaign waits for or shares the box with another
program. ms02 also satisfies section 13's dedicated-host requirement. The
primary-host designation itself stays an operator entry in the ledger; changing
it requires only fresh baselines there. Certification runs, screens, and
confirmations run at 12 workers or the host's worker sweet spot recorded in the
ledger; single-worker runs are for diagnosis only.

An experiment is invalid, not merely unsuccessful, if it changes its evaluator
after seeing its result, reads a withheld artifact, loses exact replay, or supplies
target-specific knowledge to the searcher. Host identity, affinity, and co-tenancy
are recorded for diagnosis but are never grounds for invalidation in the search
lane — determinism makes them irrelevant to the result.

## 6. Research spine

Research informs mechanisms; it is not a backlog whose every idea must be
implemented. The working spine is deliberately small:

1. **Remember and return.** Deterministic snapshots turn a one-shot sparse-reward
   problem into repeated exploration from retained states. This is the core
   Go-Explore insight.
2. **Bounded quality-diversity.** Retain diverse behavioral representatives, but
   keep the active population and its expensive snapshots bounded.
3. **Correlated rollouts.** Generate bounded sequences with temporal structure
   rather than independent frame bytes. Rollouts are generic input tactics, not
   routes.
4. **Retire by evidence after admission.** Admit mechanically valid states under
   the archive rule, then reduce energy or release them after their extensions
   prove unproductive.
5. **Separate discovery from optimization.** Exploration discovers observation
   cells and transitions. Optimization maintains best known transition or route
   costs and propagates improvements without quadratic rescanning.
6. **Keep policy off the throughput ceiling.** Snapshot transport, execution, and
   coordination must keep all usable cores occupied. A smarter selector that
   starves the executor is a regression.
7. **Learn only through a narrow optional interface.** Learned representations or
   model proposals may later compete as plugins under the same evaluator. They may
   not make the core unreplayable or target-specific.

## 7. Archive invariants

Required properties, independent of final representation:

- active search memory has a declared bound and reaches a steady state;
- replacing or retiring an entry releases its expensive snapshot unless an active
  descendant still needs it;
- action lineage is represented once rather than by a full action vector per entry;
- the append-only run stream remains the authority for replay and audit;
- a live checkpoint contains the active restart state and the minimal lineage
  needed to reconstruct its inputs, never every obsolete snapshot;
- exact replay reconstructs the same final report and verified winning input;
- no archive limit can silently freeze all future admission.

Memory accounting on scorecard 2.1 uses deterministic byte counts — archive bytes,
snapshot bytes, checkpoint bytes, entry counts — computed from the stream and
archive state. Resident set size is a systems-lane sidecar.

Names such as a separate active archive, action DAG, or replay log are
implementation possibilities, not charter-mandated abstractions. The simplest
representation that satisfies the properties wins.

## 8. Benchmark ladder

### B2 — visible level challenges

Three to seven mechanically diverse SMB levels. A challenge fixture consists only
of:

- a natural level-entry snapshot;
- the replay prefix that creates it, held by the evaluator;
- a mechanical observation schema;
- a terminal success predicate;
- a fixed action and execution budget.

The searcher receives the start snapshot and the same generic observation surface
it receives in a full campaign. It does not receive a route, obstacle locations,
hand-picked inputs, level-specific mutators, or a statement of why the level is
hard. Fixtures are development accelerators and never count toward the cold
full-game claim.

The evaluator suite is: 2-2, 7-2 (paired with 2-2 to test transfer within one
movement regime), 8-1, 4-2, 4-3 (both retained without a hand-authored failure
explanation), 7-4, and 8-4. These names and rationales are evaluator metadata only.

Level 1-1 is a fast canary, never a member of the hard-suite ranking. Every
candidate must clear it under a small frozen execution budget; failure is a
regression veto.

### B3 — visible chains

Combine adjacent level-entry and level-exit conditions into multi-level campaigns.
These expose archive carryover, changing local dynamics, transition-cost
propagation, and memory growth that isolated challenges miss.

### B4 — cold full SMB

Start from gameplay genesis with an empty archive. Run only promotion candidates.
The cold-campaign budget is a frozen count of completed worker jobs, initially
**110,000** (45 minutes at the last operator-run throughput of ~40 jobs/second at
twelve workers). The budget is recalibrated only when the systems lane certifies a
champion binary on the benchmark host, and only between evaluator versions. The
final evaluation uses the five sealed seeds and the claim levels in section 1.

### B5 — withheld suite

Withhold additional levels, ROMs or games, and non-game deterministic systems,
consulted per section 4. The same compiled generic searcher and default policy run
every withheld target. Only the action vocabulary, mechanical observations,
terminal predicates, and resource bounds may differ. Target-specific tuning voids
the transfer result.

B0 (component costs) and B1 (pipeline scaling) belong to the systems lane
(section 13).

## 9. Fixed experiment protocol

Each candidate begins with a one-screen record containing:

- identifier, round, and parent champion commit;
- one falsifiable hypothesis and its lens (scheduler, energy, splice, retention,
  mutator, archive, boundary, or another declared component);
- primary scorecard and protected secondary metrics;
- files or modules allowed to change;
- visible columns and paired seeds;
- fixed execution budgets;
- automatic kill, promotion, and invalidation rules;
- expected mechanism and predicted result;
- complexity delta expected if promoted.

The evaluator, column definitions, and budgets are immutable for the duration of a
batch. Changing them starts a new evaluator version and requires recertifying the
baseline. The 30K and 100K execution boundaries are the current screen and
confirmation budgets; E00 may recalibrate them once, before the first challenger.

### Stage 0 — static and deterministic checks

Build, format, lint, targeted tests, exact replay of a fixed micro-campaign on
the primary evaluation host, and the boundary audit: a diff scan plus reviewer
pass rejecting game constants above the observation adapter. A failure is
`INVALID` or `CRASH`; it receives no efficacy run. A simple implementation typo may
be repaired once within a bounded worker budget; a second failure kills the
candidate.

A run whose purpose is to produce an artifact — a fixture snapshot, a transition
state, a replay prefix — first passes a static check that the admission and
retention rules can emit that artifact. Executions spent generating an artifact
the code cannot record are pure waste.

### Stage 1 — screen by successive halving

Screening exists to kill cheaply, and its budget is the smallest that
discriminates — measured by the evaluator's rank-prediction record, never
assumed. The default funnel:

1. **1a:** every candidate runs 5,000 executions on the three most
   rank-discriminating challenges with one seed. The dominated half of the
   field dies here.
2. **1b:** survivors run 30,000 executions on the full visible suite with
   paired seeds. Kill when the worst challenge is dominated, there is no unique
   win, or the baseline's first-quartile progress boundary is missed.

Never watch a jammed run for hundreds of thousands of additional executions.
The per-candidate Stage-1a spend is frozen in the round record; when the round's
total screen cost exceeds one working session on the primary host, the director
cuts screen budget, challenge count, or seed count — recorded in the round
record, never silently — and never compensates by running fewer candidates.

The reduction script, its vetoes included, is frozen before the first screen and
dry-run against the champion baseline; a veto the champion itself fails is
invalid by construction. Survivor counts are bounds, never quotas — a round that
produces fewer survivors than expected proceeds with the survivors it has.

### Stage 2 — 100K confirmation

Run only the round's provisional winner (and any survivor within its noise
band) to 100,000 executions with at least six paired seeds. Confirm only if the
primary improvement repeats, no challenge regresses catastrophically,
deterministic memory bytes stay within bound, exact replay passes, and the
mechanism matches the prediction. Otherwise mark `INCONCLUSIVE` or `REJECT`.
Exact replay above micro scale is serial and expensive: the round's one
full-stream replay belongs to the confirmed winner; every other replay check
runs at micro scale.

### Stage 3 — chain and simplification

Run B3 and perform a design review. Delete obsolete paths, switches, types, and
names. Re-run Stage 2 after simplification. A candidate that requires both old and
new mechanisms to remain selectable is not ready to promote.

### Stage 4 — full campaign

Run one visible cold campaign under the frozen job budget. Only a survivor may be
submitted to the sealed seeds. A failure does not receive an improvised extension.

Statuses are `PROMOTE`, `REJECT`, `INCONCLUSIVE`, `INVALID`, and `CRASH`. Results
are append-only. Code from rejected candidates is discarded; the hypothesis,
commit, metrics, and diagnosis remain recorded.

## 10. Rounds: concurrent candidates, serial integration

The unit of concurrency is the candidate; the unit of integration is the round.
Candidates never merge with each other — they compete, and the director
integrates. The unit of scientific progress is the completed round: many small
rounds beat few large ones, and a round's evaluation is sized to complete within
one working session on the primary host.

Each round:

1. **Freeze.** The director fixes the base champion commit, the evaluator version,
   the seed list, and the budgets. These are immutable for the round.
2. **Fan out.** The director dispatches K candidate workers (K bounded by
   evaluation compute, not by token budget). Each worker gets one lens from the
   hypothesis list, an ephemeral worktree branched from the base, and the frozen
   experiment-record template. Workers do not see each other's diffs or results.
3. **Evaluate.** The deterministic controller runs every candidate through Stages
   0–2 on whatever hardware is available, in parallel. All rankings use scorecard
   2.1, worst-column aggregation.
4. **Select.** The director promotes at most one winner per component. Winners
   whose diffs touch disjoint files may be combined; the combined commit re-runs
   the Stage-2 matrix before it becomes the new champion, and the combination is
   abandoned if either winner's claim fails to repeat on it.
5. **Record.** Every candidate — winner or loser — gets a ledger row and a result
   packet. Losing code is discarded; a losing diagnosis that names a mechanism
   feeds the next round's hypothesis list.
6. **Rebase and repeat.** The next round freezes the new champion.

**Stall rule.** When the worst in-loop column's best progress fails to improve
for two consecutive rounds, the next round must include the diagnosis lens
(E08), and at least half its candidates must change a component class untried
against that column in those rounds — rollout distribution, cell
representation, archive grouping, retirement, all still generic. A stalled
column is never answered with a larger budget, more seeds, a rerun, or another
round of parameter tuning on the same component.

**The loop does not stop.** A blocked experiment, a missing fixture, or a
protocol contradiction degrades that round: the director records the deviation
in the round record, proceeds with the work that remains, and queues the
question for the operator without waiting on the answer. The director pauses
the program only when the charter itself must change. While runs are in flight
the director wakes at most every ten minutes and writes one consolidated status
per wake; heartbeat and watcher text reference ledger rows rather than
restating derived numbers or rules, so a stale copy can never contradict the
ledger.

Roles:

- **Sol director:** owns the champion branch and the ledger; selects hypotheses,
  freezes rounds, reviews architecture, interprets results, runs simplification
  passes, promotes.
- **Luna workers:** one candidate each — implementation, targeted profiling, and
  compact result reduction. Run at explicit high effort.
- **Deterministic controller:** schedules evaluation runs, enforces budgets,
  captures artifacts, kills overruns, computes result tables, and performs the
  cross-host identity check without model calls.

The director receives summaries, never raw logs: hypothesis, commit, evaluator
version, status, primary metrics, protected regressions, artifact hashes, and at
most the smallest diagnostic excerpt needed to explain failure.

Models cannot alter a running round's budget, evaluator, seed list, or promotion
threshold. They cannot read withheld fixtures. Model calls never occur inside the
search loop.

After every two to three completed rounds, the director pauses new candidates for
a simplification and synthesis pass: update the short list of beliefs and candidate
hypotheses, remove concepts made redundant by evidence, consult the withheld
columns if a promotion to `main` is proposed, and start a new batch from the
current champion.

## 11. Promotion rules

A challenger may replace the champion when all of the following hold:

- its predeclared primary metric improves at the required stage;
- exact replay, the cross-host identity check, and repository checks pass;
- no protected scorecard suffers an unexplained material regression;
- the result repeats across seeds;
- the claimed mechanism is supported by the measurements;
- the change respects the target boundary and withheld rules;
- experimental switches have been removed and the behavior is the default;
- the simplification review finds no duplicate mechanism or vocabulary;
- the result record is complete enough for another agent to reproduce.

For pure simplification, identical recorded streams are sufficient. For search
work, a gain on only one named challenge is insufficient unless the hypothesis
predeclared why that challenge isolates a generic state-space property and another
challenge confirms it.

## 12. Results and artifacts

Raw run directories are untracked and content-addressed. A compact append-only
ledger records:

```text
round  experiment  parent  candidate  evaluator  status  primary  secondary  complexity  description
```

Each raw directory contains the frozen experiment record, exact commands, host
identity, stdout/stderr, measurements, stream and report hashes, replay verdict,
and links or hashes for large checkpoints. The ledger must be cheap for an agent to
read; raw artifacts are opened only for diagnosis.

The controller is a small extension of existing campaign and benchmark binaries.
Add machinery only when a measured bottleneck or repeated operational error
requires it. Host attestation machinery — lock choreography, cgroup accounting,
patched profilers, readiness packets — is out of scope in the search lane.

## 13. Systems lane

Experiments whose primary metric is wall-clock throughput (E01–E04: hot-path
decomposition, portable-snapshot tax, recording and coordinator tax,
heterogeneous core scaling) run only on the dedicated benchmark host: fixed
governor, no co-tenants, homogeneous selected cores or measured per-core
baselines, controlled thermals. The exclusive ms02 grant satisfies this, so the
lane is open.

The lane opens by measuring the host's noise floor first: repeated identical
runs, warm-up discarded, randomized interleaved ordering, and a minimum
detectable effect frozen from the measured floor — never chosen a priori. B0 and
B1 define the lane's measurements. The lane certifies executions-per-second for
each champion binary; that number converts execution budgets to wall-time claims
and recalibrates the B4 job budget. Recalibrating B4 from ms02's certified
throughput is the lane's first action: the current 110,000-job budget was
calibrated on the retired backend at roughly 40 jobs per second, and the
QuickNES backend on `main` runs two orders of magnitude faster.

Champion certification also records two endurance numbers from a soak of at
least 2,000,000 executions at the host's sweet-spot worker count:

- **throughput endurance:** windowed throughput in the mature phase divided by
  the early-run median. The merged backend certifies at 0.92–0.95; a champion
  below 0.90 is a regression veto regardless of search-lane gains.
- **memory plateau:** peak resident set must plateau within the recorded
  `--memory-budget-mib` bound, with no swap and no reintroduced synchronous
  checkpoint rewrites in the admission loop.

## 14. First batch under this charter

The path to rounds is two steps: E00, then E07. Everything else — boundary
ratchet, memory bound, energy diagnosis — is a round lens, dispatched as a
candidate, never a prerequisite. Engineering work for a pending experiment
proceeds concurrently with any running campaign; only recorded results wait for
their prerequisites. E-numbers continue the existing ledger; E01–E04 run in the
systems lane per section 13.

### E00 — certify deterministic evaluation

**Hypothesis:** the current searcher head can serve as a champion.

Exit: the micro-campaign cross-host smoke test passes, and one 30K run at 12
workers replays exactly on the primary evaluation host. There is no second-host
30K leg and no noise-floor requirement.

### E07 — build the visible challenge suite

Instantiate B2's fixtures (2-2, 7-2, 8-1, 4-2, 4-3, 7-4, 8-4, and the 1-1
canary). A fixture is certified when its prefix replays once on the primary host
and its 12-worker baseline screen is recorded. A fixture prefix is evaluator
metadata and may be produced by any evaluator-side means, including chaining
from an earlier level's exit; a fixture whose generation run comes up empty is
recorded and backfilled later. Rounds (section 10) begin when the 1-1 canary
and at least three hard challenges are certified; remaining fixtures join the
suite at the next evaluator version. A missing fixture never blocks rounds.

### E09 — selection rounds

Open selection rounds (section 10) immediately after E07, one lens per candidate.
The round-1 hypothesis list is seeded from existing recorded diagnoses (splice
pricing, chord-table recency, corridor variance, energy collapse); a fresh
diagnosis pass is a candidate, never a prerequisite. Standing lenses available to
every round alongside algorithmic challengers:

- **boundary lens (E05):** classify `Game` methods as machine operation,
  mechanical observation, or search policy; move policy methods generic; add a
  non-SMB test target and a dependency check preventing generic modules from
  importing SMB; remove the 45-frame probe. Promotes on unchanged default
  streams, exact replay, and a falling complexity score.
- **memory lens (E06):** byte census at 10K/30K/100K, then one lifetime change
  at a time. Displaced-snapshot release, parent-relative input encoding, and the
  recorded memory budget are already the default on `main`; the lens measures
  from that baseline. Promotes on at least 30% lower mature
  archive-plus-checkpoint bytes with no search-effectiveness regression.
- **diagnosis lens (E08):** observation-only accounting for selections,
  retentions, new slots, new cells, and cost improvements by generic archive
  group. Exit is at most three falsifiable hypotheses for the next round; it
  never adds a policy.

The first predeclared algorithmic challenger: a generic two-mode scheduler — one
mode equalizing discovery across active observation cells, one mode improving
best known transition costs on the discovered connectivity graph — competing
against the current combined energy score. Promotion requires better worst-column
progress AUC or more challenge completions, bounded memory bytes, and an equal or
lower post-simplification concept count.

### Batch stopping point

After the second synthesis pass, the director produces a short evidence table, the
champion commit, deleted mechanisms, the remaining bottleneck, and no more than
three hypotheses for the next batch. No cold full-game campaign runs before E00
and E07 pass and mature archive bytes sit within a declared bound. New-game
adapter bring-up (section 4) may proceed in parallel with the whole batch.

## 15. Research basis

Primary sources and the specific lesson borrowed from each:

- [Antithesis: Super Mario Bros. in about 45 minutes](https://antithesis.com/blog/sdtalk/)
  establishes the external target, minimal x/y/level hints, and same-configuration
  transfer to a Kaizo ROM.
- [Antithesis: Depth is all you need](https://antithesis.com/blog/2025/gradius/)
  separates input tactics from state-evaluation strategy and explains why
  correlated rollouts cross local dips.
- [Antithesis: They don't even have eyes](https://antithesis.com/blog/2025/alchemical_intelligence/)
  reinforces that deterministic rewind permits simple tactics instead of a complex
  learned player.
- [Antithesis: Optimizing our way through Metroid](https://antithesis.com/blog/2025/metroid/)
  motivates bounded behavioral cells, a connectivity graph, best transition costs,
  incremental propagation, and keeping policy fast enough to saturate execution.
- [Go-Explore: First return, then explore](https://arxiv.org/abs/2004.12919)
  supplies explicit state memory and return-before-explore.
- [When to Go, and When to Explore](https://arxiv.org/abs/2203.16311)
  motivates measuring when and how long to explore after return.
- [Cell-Free Latent Go-Explore](https://proceedings.mlr.press/v202/gallouedec23a.html)
  and [Time-Myopic Go-Explore](https://arxiv.org/abs/2301.05635) show that cell
  representations are a real failure surface.
- [Quality with Just Enough Diversity](https://arxiv.org/abs/2405.04308) and
  [Quality-Diversity with Limited Resources](https://proceedings.mlr.press/v235/wang24cd.html)
  treat archive resource cost as part of the algorithm.
- [Nyx-Net](https://arxiv.org/abs/2111.03013) demonstrates that incremental
  snapshot design transforms stateful-search throughput.
- [Stateful Greybox Fuzzing](https://arxiv.org/abs/2204.02545),
  [AlphaFuzz](https://arxiv.org/abs/2101.00612), and
  [Mallory](https://arxiv.org/abs/2305.02601) provide transferable evidence for
  derived state abstractions, lineage-aware scheduling, and adaptive feedback on
  non-game systems.
- [Karpathy's autoresearch program](https://github.com/karpathy/autoresearch/blob/master/program.md?plain=1)
  supplies the fixed evaluator, fixed short budget, append-only result ledger,
  keep-or-discard progression, hard timeout, and autonomous loop.
- [AlphaEvolve](https://deepmind.google/discover/blog/alphaevolve-a-gemini-powered-coding-agent-for-designing-advanced-algorithms/)
  and [FunSearch](https://www.nature.com/articles/s41586-023-06924-6) pin the
  intended reading of section 10: many candidates from one frozen parent, an
  automatic evaluator, selection as the integration mechanism, discarded losers.

## 16. Start condition

The program begins only after a human approves this charter and requests the
research-director task. That task starts from the approved charter commit, creates
the batch branch and untracked artifact root, and executes E00. It may not skip
directly to an algorithm change or a full-game campaign.
