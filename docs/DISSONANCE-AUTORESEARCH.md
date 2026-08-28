# Dissonance autoresearch charter

**Status: governing research and execution plan for Dissonance search work.**

This charter controls work whose purpose is to improve Dissonance's search speed,
resource efficiency, or generality. It supersedes
`docs/MODEL-IN-THE-LOOP-PLAN.md` as the current execution plan. The from-scratch
design and LibAFL plan remain background for the model boundary, target seam, and
established vocabulary; where an old milestone or mechanism conflicts with this
charter, this charter wins.

The program has one concrete target: from a cold Super Mario Bros. gameplay
genesis, with no previously discovered corpus or route and no game knowledge beyond
the declared mechanical observations, find and replay a complete end-to-end run in
less than 45 minutes on `msr1` under the active evaluator's frozen hardware
allocation. This is an engineering target and a research benchmark, not permission
to specialize the searcher for Mario.

Here **autoresearch** means an agent improving the mechanical search implementation
between fixed experiments. It does not mean an LLM participating in a campaign.
Model triage, generated detectors, generated mutators, trajectory seeding, and the
previous model-in-the-loop outer design are outside this program. They remain frozen
unless a later charter reintroduces one through the same evaluator after the
mechanical core is fast, bounded, generic, and understood.

### Current benchmark allocation: evaluator-v2-msr1-big

The first batch now runs as `evaluator-v2-msr1-big`. Preserve `evaluator-v1`
and all E00 through E00R6 artifacts as immutable `INVALID` history; do not
reinterpret or overwrite them. Restart E00 from a fresh artifact root:

- local: `target/autoresearch/evaluator-v2-msr1-big-batch-1`;
- msr1: `/root/harmony-autoresearch/evaluator-v2-msr1-big-batch-1`.

This evaluator reserves msr1's eight Cortex-A720 CPUs `0,1,6-11` for
Dissonance. Its canonical promotion grid and affinity sets are W1 `0`, W4
`0,1,10,11`, and W8 `0,1,6-11`; evaluator-v2 has no W2 promotion point. Run
the controller in an auditable systemd cgroup with `AllowedCPUs=0,1,6-11`,
and run every measured child in an auditable child cgroup whose `AllowedCPUs`
equals its declared set. The controller and child effective affinities read
from `/proc` must equal, not merely be subsets of, their declared sets. The W4
set is the four highest-capacity cores; W8 is the complete big-core partition.

Consonance owns the four Cortex-A520 CPUs `2-5`. Consonance `cargo`, `rustc`,
tests, image builds, and KVM runs are permitted concurrently only when their
systemd cgroup has `AllowedCPUs=2-5` and their observed effective affinity is
exactly `2-5`. The controller scans external workload processes in user
sessions and non-evaluator build, test, benchmark, and transient-service
cgroups. E00 freezes and hashes the discovery implementation and the fixed
boot-service exclusions before any measurement; kernel threads and fixed boot
services are recorded but are not classified as external workloads. Preflight
and the live monitor reject an external workload process when its effective
allowed CPU set intersects the Dissonance reservation; a non-overlapping
process is not rejected merely because its executable is named `cargo` or
`rustc`. Record every discovered process's PID, command, cgroup, effective
affinity, classification, and decision so the result is auditable.

Before readiness, the controller must prove this gate with planted controls.
An external workload confined to a set containing CPU 0 must be detected and
must invalidate its sacrificial sample. A real `cargo` process from the frozen
Consonance source, confined to `2-5`, must be detected and recorded but must
not invalidate its sacrificial sample. A measured child whose cgroup
`AllowedCPUs` is correct but whose `/proc` effective affinity is narrower than
the declared set must be rejected. Preserve PID, argv, cgroup properties,
effective affinity, monitor decision, and controller terminal evidence for
all three controls. Efficacy may not begin unless all three behave as
specified.

CPU affinity alone is not evidence of benchmark isolation because L3, memory
bandwidth, storage, power, and thermals remain shared. Before its first sample,
E00 freezes the representative Consonance compile/test load's exact source
commit, lockfile hash, command, environment, target directory, and wall or
completion bound. It also freezes an immutable initial target/cache-state
archive and hash. Before every loaded arm, restore that archive into a fresh
target directory, verify its hash, and perform the same frozen warm-up before
measurement, so all three repetitions start from the same build state. For
each canonical W1/W4/W8 cell, run three otherwise-idle and three loaded samples
with the same paired seeds in the fixed alternating order idle, loaded, idle,
loaded, idle, loaded. Report each arm and the pooled cell distribution.
Concurrent operation is allowed only if every arm and pooled cell meets E00's
`1/60` CV ceiling and the absolute loaded-versus-idle median shift does not
exceed the protected-metric tolerance computed from the maximum relative MAD
across all concurrency-gate distributions. Otherwise the evaluator freezes
exclusive timing. This decision is frozen for E00-E04 and cannot be relaxed
after challenger results are visible.

The reservation primitive is `/run/lock/harmony-msr1-benchmark.lock`. Every
Consonance compute job holds a shared `flock` for its complete lifetime and an
exclusive `/run/lock/harmony-msr1-consonance-compute.lock` to serialize
Consonance compute jobs. While the concurrency decision is being measured,
each idle arm holds the benchmark lock's exclusive side across preflight and
measurement; each loaded arm holds the shared side, and exactly one frozen
Consonance load holds another shared acquisition for the arm's complete
lifetime. If shared timing passes, only one Consonance job matching the frozen
source, command, environment, initial target/cache state, and bound may overlap
a timed Dissonance sample; any other Consonance job or additional shared holder
invalidates that sample. After the decision, every timed Dissonance sample
holds the shared side if concurrent timing passed or the exclusive side if it
failed. Record requested mode, request time, acquisition time, release time,
and the identities of all observed lock holders in every sample manifest.

## 1. What is being optimized

The headline clock starts after the release binary receives the ROM and before it
constructs its first target. It stops when the search first records an input that a
separate mechanical verifier replays from genesis to the victory state. Compilation,
fixture creation, and prior campaigns are outside the clock; target initialization,
search, snapshotting, coordination, recording, and victory verification are inside
it.

One lucky run is useful evidence, but not a publishable result. We use three claim
levels:

1. **Breakthrough:** at least one of five predeclared, sealed campaign seeds completes
   in less than 45 minutes on `msr1`.
2. **Engineering target:** at least four of those five seeds complete and their median
   time to verified victory is less than 45 minutes.
3. **Transfer claim:** the same compiled searcher and default policy improves the
   sealed cross-level and cross-target suite without target-specific tuning.

The final claim reports every seed, including failures. A full campaign starts from
genesis and may not import an archive, snapshot checkpoint, action sequence, learned
route, or model-generated trajectory.

The 45-minute clock is not the metric used for ordinary iteration. It is the last
gate, reserved for candidates that have already won short, paired evaluations.

## 2. The three scorecards

Every experiment belongs primarily to one scorecard. A change may affect the other
two, but it must not hide a regression there.

### 2.1 Search effectiveness

Measured with integer counters and deterministic campaign events:

- executions and emulated frames to each mechanical progress milestone;
- time to first verified victory;
- success count across predeclared seeds;
- area under the best-progress-by-execution curve;
- retained descendants per 1,000 parent selections;
- new archive slots and new observation cells per 1,000 executions;
- longest barren selection streak before new progress;
- work spent revisiting, replacing, or extending nonproductive states;
- best known input cost to each reached observation cell.

Wall time is reported separately. It never enters a search decision.

### 2.2 Systems performance

Measured on named hardware, release builds, fixed affinity, and repeated runs:

- raw emulated frames per second;
- complete worker jobs per second;
- snapshot, restore, export, import, read, and observation cost;
- coordinator service time per completed job;
- bytes copied, hashed, encoded, and written per job;
- resident memory and checkpoint bytes as the archive matures;
- active entries versus historical entries;
- throughput at 1, 4, and 8 workers;
- worker idle fraction and coordinator saturation;
- heterogeneous-core scaling efficiency.

For a set of cores `C`, heterogeneous ideal throughput is the sum of each selected
core's isolated one-worker throughput. Scaling efficiency is concurrent throughput
divided by that sum. This avoids pretending `msr1`'s big and little cores are equal.
The initial acceptance floor is 70% at 8 workers; the program target is at least
85% unless measurement identifies an unavoidable shared bottleneck.

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
generic searcher  ->  machine interface  <-  NES implementation
       ^
       |
mechanical observation adapter (SMB today, other targets later)
```

The boundaries mean:

- **The searcher is target-blind.** It may know actions, observations, snapshots,
  terminals, deterministic cost, and generic archive relationships. It may not know
  Mario, screens, rooms, pipes, levels, controller buttons, NES memory addresses,
  distributed-system events, database operations, or target-specific policy names.
- **The machine implementation is search-blind.** NES implements the machine verbs
  and efficient state transport. It does not decide novelty, quality, admission,
  parent selection, mutation energy, retirement, or progress.
- **The SMB layer is mechanical.** It may define the controller action vocabulary,
  boot-to-gameplay genesis, observations such as world/level/x/y, and mechanical
  terminal states such as death and victory. It may not choose archive grouping,
  admission filters, selection weights, rollout distributions, retirement rules, or
  search phase changes.
- **The evaluator may understand the benchmark.** Level fixtures, milestone scoring,
  and victory verification live outside the production searcher. Evaluation knowledge
  must never flow back into a run.

The acceptance test is structural as well as behavioral: generic search modules must
not import `smb`; target modules must not implement search policies. A new target
should require a machine or target adapter, an action vocabulary, a mechanical
observation schema, terminal predicates, and resource bounds—not a new scheduler.

The current `main` at `d09d9d38` is the starting point, not proof that the boundary is
finished. `dissonance/machine` now mirrors the consonance-shaped machine interface,
and `dissonance/searcher/src/search` holds generic coordination and archive code.
However, the current `Game` trait still delegates suffix expansion, archive evidence,
candidate completion, job execution, and draw-state evolution to SMB; SMB also owns
archive grouping and retains the 45-frame admission probe as a selectable legacy
mechanism. Boundary work is complete only when those search decisions have moved to
generic code or have been demonstrated to be irreducibly mechanical observations.

Prior operator runs are useful planning evidence but are not yet a certified
baseline: roughly 34 jobs/second at four workers and 40 jobs/second at twelve workers;
one 50K-job run emulated about 2.84 million frames while its inherited plus new
archive held about 322K entries and 1.47 GB of snapshots. Because those artifacts are
not part of this branch's evaluator, E00 must reproduce the measurements before an
experiment uses them for promotion.

## 4. Determinism and honesty

The repository's determinism contract applies without exception. In particular:

- same recorded stream plus recorded external artifacts must replay byte-identically;
- all state-affecting randomness comes from recorded or derivable seeds;
- no wall clock, floating point, host entropy, unordered iteration, model response, or
  thread timing enters replayable campaign state;
- live worker completion order may choose the recorded stream, but that stream is the
  run identity and must replay exactly;
- benchmark wall time, CPU counters, and process memory are observation-only sidecars;
- failed, timed-out, and killed experiments remain in the result ledger;
- no experiment is rerun with a larger budget merely because an agent believes it
  might improve later.

An experiment is invalid, not merely unsuccessful, if it changes its evaluator after
seeing its result, reads a held-out artifact, silently changes affinity or hardware,
loses exact replay, or supplies target-specific knowledge to the searcher.

## 5. Research spine

Research informs mechanisms; it is not a backlog whose every idea must be
implemented. The working spine is deliberately small:

1. **Remember and return.** Deterministic snapshots turn a one-shot sparse-reward
   problem into repeated exploration from retained states. This is the core
   Go-Explore insight.
2. **Bounded quality-diversity.** Retain diverse behavioral representatives, but keep
   the active population and its expensive snapshots bounded. More descriptor
   dimensions are not automatically better.
3. **Correlated rollouts.** Generate bounded sequences with temporal structure rather
   than independent frame bytes. Rollouts are generic input tactics, not routes.
4. **Retire by evidence after admission.** Admit mechanically valid states under the
   archive rule, then reduce energy or release them after their extensions prove
   unproductive. Do not predict doom with target-specific lookahead before admission.
5. **Separate discovery from optimization.** Exploration discovers observation cells
   and transitions. Optimization maintains best known transition or route costs and
   propagates improvements without quadratic rescanning. A single unexplained score
   should not blur the two jobs.
6. **Keep policy off the throughput ceiling.** Snapshot transport, execution, and
   coordination must keep all usable cores occupied. A smarter selector that starves
   the executor is a regression.
7. **Learn only through a narrow optional seam.** Learned representations or model
   proposals may later compete as plugins under the same evaluator. They are not the
   foundation and may not make the core unreplayable or target-specific.

The 2026 uncertainty-guided tree-search work and learned Go-Explore representations
are research candidates, not assumed improvements. They enter only after the simpler
state, transition, and cost data structures are measured and after the visible suite
can distinguish a gain from noise.

## 6. Archive invariants

The current archive keeps every admitted entry, its reconstructed full input, and its
snapshot even after replacement makes it inactive. That makes historical lineage,
the active search population, replay evidence, and checkpoint material one growing
object. The research program must separate their lifetimes without multiplying
concepts unnecessarily.

Required properties, independent of final representation:

- active search memory has a declared bound and reaches a steady state;
- replacing or retiring an entry releases its expensive snapshot unless an active
  descendant still needs it;
- action lineage is represented once rather than by a full action vector per entry;
- the append-only run stream remains the authority for replay and audit;
- a live checkpoint contains the active restart state and the minimal lineage needed
  to reconstruct its inputs, not every obsolete snapshot;
- exact replay reconstructs the same final report and verified winning input;
- no archive limit can silently freeze all future admission.

Names such as a separate active archive, action DAG, or replay log are implementation
possibilities, not charter-mandated abstractions. The simplest representation that
satisfies the properties wins.

## 7. Benchmark ladder

### B0 — component costs

Run on one pinned core with target state at genesis and at a mature checkpoint:

1. raw frame stepping with rendering disabled;
2. one-byte and whole-observation reads;
3. in-instance snapshot and restore by handle;
4. portable snapshot export, import, and restore;
5. one action including mechanical observation collection;
6. one complete worker job with 1, 3, and 6 actions;
7. candidate digest and stream encoding;
8. archive selection and admission at empty, middle, and mature sizes;
9. live checkpoint generation at those sizes.

Each operation reports median, median absolute deviation, bytes touched, and sample
count. The benchmark binary must have less than 2% overhead when campaign counters are
enabled but detailed timing is disabled.

### B1 — pipeline scaling

Use an immutable origin and deterministic job set to measure 1, 4, and 8 workers.
Record per-core isolated throughput first, then concurrent throughput,
coordinator utilization, worker idle fraction, memory bandwidth counters when
available, and job-size distribution. Run at empty and mature archive states.

### B2 — visible level challenges

Use three to seven mechanically diverse SMB levels. A challenge fixture consists only
of:

- a natural level-entry snapshot;
- the replay prefix that creates it, held by the evaluator;
- a mechanical observation schema;
- a terminal success predicate;
- a fixed action and execution budget.

The searcher receives the start snapshot and the same generic observation surface it
receives in a full campaign. It does not receive a route, obstacle locations,
hand-picked inputs, level-specific mutators, or a statement of why the level is hard.
Fixtures are development accelerators and never count toward the cold full-game claim.

`evaluator-v2-msr1-big` inherits the evaluator-v1 visible workload suite unchanged:

- **2-2:** the first water level;
- **7-2:** the later water level, paired with 2-2 to test transfer within one movement
  regime;
- **8-1:** a long level with strong time pressure;
- **4-2:** deliberately left without a hand-authored failure explanation;
- **4-3:** deliberately left without a hand-authored failure explanation, so the suite
  retains challenges whose value is empirical rather than fitted to our theory;
- **7-4:** a late castle maze;
- **8-4:** the final level and a composition test for mechanics learned elsewhere.

These names and rationales are evaluator metadata only. Start with rotating parallel
waves of up to three challenges allocated four logical cores each, including
coordination. After B1/E04 measures the real coordinator cost, the controller may use
smaller allocations and run more challenges concurrently. This gives broad failure
coverage before spending 45 minutes on one trajectory without assuming all twelve
cores are equivalent.

Level 1-1 is a fast canary, not a member of the hard-suite ranking. Every candidate
must clear it under a small frozen budget; failure is a regression veto. The canary
checks that work aimed at difficult levels did not make ordinary rightward traversal
and jumping needlessly expensive.

### B3 — visible chains

Combine adjacent level-entry and level-exit conditions into multi-level campaigns.
These expose archive carryover, changing local dynamics, transition-cost propagation,
and memory growth that isolated challenges miss.

### B4 — cold full SMB

Start from gameplay genesis with an empty archive. Run only promotion candidates.
The first gate is a 45-minute visible-seed run; the final evaluation uses the five
sealed seeds and claim levels in section 1.

### B5 — sealed transfer suite

Withhold additional levels, ROMs or games, and non-game deterministic systems. Agents
may see only aggregate pass/fail and normalized metrics after a candidate is frozen.
They may not inspect fixture identities, snapshots, traces, or per-case failures.

The same compiled generic searcher and default search policy run every sealed target.
Only the action vocabulary, mechanical observations, terminal predicates, and resource
bounds may differ. Target-specific search tuning voids the transfer result.

## 8. Fixed experiment protocol

Each experiment begins with a one-screen record containing:

- identifier and parent champion commit;
- one falsifiable hypothesis;
- primary scorecard and protected secondary metrics;
- files or modules allowed to change;
- visible workloads and paired seeds;
- fixed execution and wall budgets;
- automatic kill, promotion, and invalidation rules;
- expected mechanism and predicted result;
- complexity delta expected if promoted.

The evaluator and workload definitions are immutable for the duration of a batch.
Changing them starts a new evaluator version and requires recertifying the baseline.
The 30K and 100K boundaries are evaluator-v1 budgets, chosen because prior failures
were visibly jammed by about 30K and the existing campaign work used 100K as its
confirmation scale. E00 and E07 may recalibrate them once, before the first
challenger; no agent may move them after results arrive.

`evaluator-v2-msr1-big` inherits the 30K and 100K execution budgets unchanged.
Its new hardware allocation, W1/W4/W8 worker grid, partition-aware isolation
monitor, and shared-resource noise gate are fixed by the current benchmark
allocation above. Any further change to those definitions requires another
evaluator version and another E00 certification.

E00 also freezes the systems minimum detectable effect as the greater of 5% and three
times the baseline relative median absolute deviation. Protected-metric tolerance is
the greater of 2% and twice that deviation. Later percentages in this batch are
minimum material effects, not substitutes for the measured noise floor.

### Stage 0 — static and deterministic gates

Build, format, lint, targeted tests, exact replay, and any Miri requirement. A failure
is `INVALID` or `CRASH`; it receives no efficacy run. A simple implementation typo may
be repaired once within a 15-minute worker budget. A second failure kills the
experiment.

### Stage 1 — component or 30K screen

For a systems hypothesis, run the relevant B0/B1 measurement with at least five
alternating baseline/challenger samples. Kill if the median primary metric fails to
clear the frozen minimum detectable effect or a protected metric exceeds its frozen
regression tolerance.

For a search hypothesis, run paired seeds to 30,000 executions on the visible level
challenges. Kill when the challenger is dominated on at least four of six challenges,
produces no unique win, or fails to reach the baseline's first-quartile progress by the
fixed boundary. Never watch a jammed run for hundreds of thousands of additional
executions.

### Stage 2 — 100K confirmation

Run only Stage-1 survivors to 100,000 executions with at least six paired seeds,
alternating run order. Promote only if the primary improvement repeats, no challenge
regresses catastrophically, memory stays within its bound, exact replay passes, and the
mechanism matches the prediction. Otherwise mark `INCONCLUSIVE` or `REJECT`.

### Stage 3 — chain and simplification

Run B3 and perform a design review. Delete obsolete paths, switches, types, and names.
Re-run Stage 2 after simplification. A candidate that requires both old and new
mechanisms to remain selectable is not ready to promote.

### Stage 4 — full campaign

Run one visible cold campaign with a hard 45-minute cutoff. Only a survivor may be
submitted to the sealed seeds. A failure does not receive an improvised extension.

Statuses are `PROMOTE`, `REJECT`, `INCONCLUSIVE`, `INVALID`, and `CRASH`. Results are
append-only. Code from rejected experiments is discarded; its hypothesis, commit,
metrics, and diagnosis remain recorded.

## 9. Promotion rules

A challenger may replace the champion when all of the following hold:

- its predeclared primary metric improves at the required stage;
- exact replay and repository gates pass;
- no protected scorecard suffers an unexplained material regression;
- the result repeats across seeds or alternating samples;
- the claimed mechanism is supported by the measurements;
- the change respects the target boundary and held-out rules;
- experimental switches have been removed and the behavior is the default;
- the simplification review finds no duplicate mechanism or vocabulary;
- the result record is complete enough for another agent to reproduce.

For pure simplification, identical recorded streams and no meaningful throughput loss
are sufficient. For systems work, search decisions must remain identical under replay
or the change is reclassified as a search experiment. For search work, a gain on only
one named level is insufficient unless the hypothesis predeclared why that challenge
isolates a generic state-space property and another challenge confirms it.

## 10. Autoresearch organization

Use one persistent research-director task, not one user-visible task per experiment.
The task operates on a dedicated `autoresearch/<date-or-batch>` branch. Each risky
experiment uses an ephemeral worktree or commit and returns a compact result packet.

Roles:

- **Sol director:** selects hypotheses, checks research grounding, freezes experiment
  records, reviews architecture, interprets results, runs simplification passes, and
  promotes the champion.
- **Luna xhigh worker:** default implementation, profiling, benchmark execution, and
  compact result reduction.
- **Luna max worker:** reserved for subtle deterministic concurrency, snapshot, or
  archive refactors where additional reasoning is worth the cost.
- **Deterministic controller:** schedules hardware, enforces affinity and budgets,
  captures artifacts, kills overruns, and computes result tables without model calls.

The director receives summaries, not raw logs: hypothesis, commit, evaluator version,
status, primary metrics, protected regressions, artifact hashes, and at most the
smallest diagnostic excerpt needed to explain failure. Repeated repository context,
full source files, build logs, and campaign streams remain on disk.

Models cannot alter a running experiment's budget, evaluator, seed list, or promotion
threshold. They cannot read sealed fixtures. Model calls never occur inside the search
loop or benchmark clock.

After every six to ten completed experiments, the director pauses new implementation
for a simplification and synthesis pass. It updates the short list of beliefs and
candidate hypotheses, removes concepts made redundant by evidence, and starts a new
batch from the current champion. Research papers are a menu for hypotheses, not a
requirement to accumulate mechanisms.

## 11. Results and artifacts

Raw run directories are untracked and content-addressed. A compact append-only ledger
records:

```text
experiment  parent  candidate  evaluator  status  primary  secondary  complexity  description
```

Each raw directory contains the frozen experiment record, exact commands, environment
and topology, stdout/stderr, measurements, stream and report hashes, replay verdict,
and links or hashes for large checkpoints. The ledger must be cheap for an agent to
read; raw artifacts are opened only for diagnosis.

The initial controller should be a small extension of existing campaign and benchmark
binaries, not a service, database, dashboard, plugin system, or second orchestration
framework. Add machinery only when a measured bottleneck or repeated operational error
requires it.

## 12. First experiment batch

The first batch establishes a trustworthy evaluator and attacks the two demonstrated
ceilings: target-flow throughput and unbounded archive cost. It also creates the
diagnostic evidence needed for the first principled search-policy change. The order is
binding because later experiments depend on earlier measurements.

### E00 — certify the landed baseline

**Hypothesis:** `d09d9d38` can be reproduced and measured consistently enough to serve
as an experiment parent.

Work:

- build the release binaries from a clean worktree;
- record compiler, kernel, topology, governor, affinity, ROM hash, and commit;
- run exact replay on a short 1-, 4-, and 8-worker campaign;
- alternate three baseline samples at each worker count;
- run once from genesis and once from the largest available mature checkpoint;
- record throughput, progress, archive size, snapshot bytes, RSS, and per-worker jobs.
- run the idle-versus-pinned-Consonance-load noise gate defined by
  `evaluator-v2-msr1-big`, then freeze whether timed samples may share msr1.

Exit: all replays are exact; coefficients of variation are low enough to distinguish a
5% systems change; all controller/child cgroup and effective-affinity checks pass; all
three planted partition controls behave as specified; and the concurrency gate
completes and freezes its lock mode with complete lock evidence. E01 may not begin
until every condition passes. Otherwise the batch stops and measurement noise is
diagnosed.

### E01 — decompose the NES and campaign hot path

**Hypothesis:** the current roughly 40-execution/second ceiling is outside raw NES frame
emulation and is attributable to one or more snapshot transport, observation, result
digest, archive, or coordinator stages.

Work: extend the existing `smb-bench` rather than add another benchmark framework. Run
B0 at genesis and a mature state. Add scoped aggregate counters to the campaign for
bytes and operation counts; keep detailed timers out of the hot path.

Exit: at least 90% of single-worker wall cost and the first multi-worker saturation
point are explained. If not, use sampling profiling before changing code.

### E02 — measure the portable-snapshot tax

**Hypothesis:** copying a portable snapshot into `NesMachine`, copying it again to
branch, decoding it, and cloning snapshots across coordinator channels materially
limits throughput and scaling.

Compare, without changing search decisions:

- in-instance handle restore;
- current portable `SmbSnapshot` restore;
- portable restore with one worker-local import/cache;
- shared immutable snapshot bytes across dispatch.

This experiment first measures variants in the benchmark. Implement the smallest
generic change only if a variant improves complete job throughput by at least 10% and
preserves exact replay. Do not expose an NES-specific fast path to the searcher.

### E03 — measure recording and coordinator tax

**Hypothesis:** serializing the full job result, including snapshots, to JSON solely to
hash it, plus serial archive admission and stream work, starves workers as core count
increases.

Use the same predetermined job set to measure execution-only, execution plus result
digest, execution plus admission, and full recorded campaign modes. Compare the
current full-buffer JSON digest with an incremental canonical digest that binds the
same logical fields and snapshot bytes without allocating the encoded result.

Promote only a format-versioned representation whose replay verifier catches a planted
snapshot-byte mutation. A faster digest that weakens evidence is invalid.

### E04 — recover heterogeneous core scaling

**Hypothesis:** after the dominant E02/E03 cost is removed, one coordinator can keep
the selected msr1 big-core partition at or above 70% heterogeneous scaling
efficiency.

Run B1 with affinity sets chosen from measured isolated-core throughput, not CPU
numbers guessed from topology. The promotion comparator uses the frozen W1/W4/W8
big-core sets. Little-only and mixed-core measurements may be retained as diagnostic
artifacts, but they run outside the timed promotion comparator and may not borrow
Consonance's CPUs during a Consonance job. Inspect queue depth and worker idle time
before proposing additional coordinators or sharded archives.

If one coordinator remains the bottleneck, the next hypothesis must preserve one
deterministic admission order—for example, bounded batches with deterministic merge.
Do not shard semantic state merely to improve a chart.

### E05 — ratchet the target boundary and remove admission probing

**Hypothesis:** the clearest target/search ownership violations can be removed without
changing default campaign decisions; the 45-frame probe is duplicate policy, not
useful architecture, and the live system is simpler with unconditional mechanically
valid admission followed by ordinary archive replacement and productivity retirement.

The current default already uses `admit_alive`, so this is a simplification experiment,
not a speed claim. First classify every current `Game` method as machine operation,
mechanical observation, or search policy. Move any search-policy method that can be
made generic without inventing a new abstraction. Add a small non-SMB test target that
exercises the resulting generic path and a dependency check that prevents generic
search modules from importing SMB.

Remove the probe from live configuration, SMB execution, tests, and policy vocabulary.
Preserve old recordings only if a named compatibility contract requires it; in that
case isolate a replay-only decoder with no live branch. Otherwise git history is the
reproducer for obsolete experimental streams. If a `Game` method cannot yet move,
record the exact missing generic input and leave one existing seam; do not wrap it in
a second interface.

Exit: default campaign streams and archive reports are unchanged, replay is exact, a
second mechanical target uses the generic path without a target-specific scheduler,
the ownership table has no unexplained policy delegation, and the complexity score
falls.

### E06 — bound active archive memory

**Hypothesis:** inactive snapshots and repeated full inputs are responsible for most
mature-run memory and checkpoint growth, and can be released without changing future
selection or admission.

First generate a byte census by category at 10K, 30K, and 100K executions. Then make
one minimal lifetime change at a time:

1. release snapshots of displaced entries with no active descendant dependency;
2. encode retained inputs by parent plus suffix rather than a full vector in memory;
3. write live checkpoints from active restart state plus required lineage.

Each step must replay the same stream to the same report and verified winning input.
Stage-1 promotion requires at least 30% lower mature RSS or checkpoint bytes with no
more than 2% throughput regression. The archive must reach a measurable steady-state
active footprint before full-game work resumes.

### E07 — build the visible challenge suite

**Hypothesis:** seven level-entry challenges expose distinct failure modes early enough
that 30K-execution screens predict which ideas deserve 100K and full-game runs.

Instantiate B2's fixed visible levels, inherited unchanged by
`evaluator-v2-msr1-big`: 2-2, 7-2, 8-1, 4-2, 4-3, 7-4, and 8-4, plus the 1-1
canary. The 2-2/7-2 pair measures transfer within water movement; 8-1 stresses
long-horizon progress and time pressure; 7-4 and 8-4 stress late-game composition;
4-2 and 4-3 remain intentionally unclassified. The evaluator records these
rationales; the searcher sees none of them.

For each fixture, certify deterministic construction, replay, terminal detection, and
baseline distributions. Measure whether the 30K ranking predicts the 100K ranking for
existing generic policy variants. Replace a challenge only between evaluator versions.

### E08 — diagnose search energy, without adding a policy

**Hypothesis:** the mature search fails because energy remains assigned to archive
regions whose observed probability of producing new cells or cheaper useful
transitions has collapsed.

Add observation-only accounting for parent selections, retained descendants, new
slots, new cells, transition endpoints, and cost improvements by generic archive
group. Run the visible suite under the current champion. Locate when forward progress
stops and whether the cause is lack of executable tactics, loss of stepping stones,
archive dilution, or repeated optimization of irrelevant regions.

Exit: produce at most three falsifiable search hypotheses. Do not implement a new
scheduler in E08.

### E09 — first algorithmic challenger: separate discovery from cost optimization

**Prerequisite:** E08 shows both continued cell discovery and wasted recomputation of
known routes or transitions. If it does not, select the strongest E08 hypothesis
instead and record why this experiment was skipped.

**Hypothesis:** a generic two-mode scheduler—one mode equalizing discovery across
active observation cells, one mode improving best known transition costs on the
discovered connectivity graph—outperforms the current combined
energy/frontier/cheapest score while using fewer policy concepts.

The challenger must:

- use only generic archive keys, observed transitions, deterministic costs, and
  productivity counters;
- maintain best known transition costs incrementally;
- propagate improvements without a whole-archive quadratic scan;
- predeclare the deterministic rule that allocates work between discovery and
  optimization;
- replace, rather than layer on top of, redundant selector machinery if promoted.

Run the 30K screen across all visible challenges, then the 100K paired-seed
confirmation. Promotion requires better progress AUC or more challenge completions,
no catastrophic challenge loss, bounded memory, and an equal or lower post-
simplification concept count.

### Batch-one stopping point

After E09—or after six completed experiments if earlier results change its
prerequisite—the Sol director performs the first synthesis pass. The output is a short
evidence table, the champion commit, deleted mechanisms, remaining bottleneck, and no
more than three hypotheses for batch two. No cold full-game campaign runs before E00,
E04, E06, and E07 pass. The first such run is a deliberate promotion gate, not a way
to debug the harness.

## 13. Research basis

Primary sources and the specific lesson borrowed from each:

- [Antithesis: Super Mario Bros. in about 45 minutes](https://antithesis.com/blog/sdtalk/)
  establishes the external target, minimal x/y/level hints, and same-configuration
  transfer to a Kaizo ROM.
- [Antithesis: Depth is all you need](https://antithesis.com/blog/2025/gradius/)
  separates input tactics from state-evaluation strategy, argues for minimal target
  knowledge, and explains why correlated rollouts cross local dips.
- [Antithesis: They don't even have eyes](https://antithesis.com/blog/2025/alchemical_intelligence/)
  reinforces that deterministic rewind permits simple tactics instead of a complex
  learned player.
- [Antithesis: Optimizing our way through Metroid](https://antithesis.com/blog/2025/metroid/)
  motivates bounded behavioral cells, quality within a cell, a connectivity graph,
  best transition costs, incremental propagation, and keeping policy fast enough to
  saturate execution.
- [Go-Explore: First return, then explore](https://arxiv.org/abs/2004.12919)
  supplies explicit state memory and return-before-explore as the hard-exploration
  foundation.
- [When to Go, and When to Explore](https://arxiv.org/abs/2203.16311)
  motivates measuring when and how long to explore after return rather than assuming a
  fixed rollout regime.
- [Cell-Free Latent Go-Explore](https://proceedings.mlr.press/v202/gallouedec23a.html)
  and [Time-Myopic Go-Explore](https://arxiv.org/abs/2301.05635) show that cell
  representations are a real failure surface and that learned representations are a
  later candidate—not license to put target semantics in the core.
- [Quality with Just Enough Diversity](https://arxiv.org/abs/2405.04308) motivates
  focusing evaluation on behavior regions that contribute to the best solution rather
  than paying indefinitely for maximum diversity.
- [Quality-Diversity with Limited Resources](https://proceedings.mlr.press/v235/wang24cd.html)
  treats archive resource cost as part of the algorithm rather than an implementation
  afterthought.
- [OMNI](https://arxiv.org/abs/2306.01711) and
  [Quality Diversity through Human Feedback](https://proceedings.mlr.press/v235/ding24h.html)
  are more recent work from members of the Go-Explore team on model-guided
  interestingness and learned diversity. They are relevant to a future outer research
  seam, but are deliberately not mechanisms in the first mechanical-search program.
- [Nyx-Net](https://arxiv.org/abs/2111.03013) demonstrates that incremental snapshot
  design can transform both stateful-fuzzing throughput and Mario search speed.
- [Stateful Greybox Fuzzing](https://arxiv.org/abs/2204.02545),
  [AlphaFuzz](https://arxiv.org/abs/2101.00612), and
  [Mallory](https://arxiv.org/abs/2305.02601) provide transferable evidence for
  automatically derived state abstractions, lineage-aware scheduling, and adaptive
  feedback on non-game systems.
- [Decoupling Exploration and Policy Optimization](https://arxiv.org/abs/2603.22273)
  is current preprint evidence for explicit separation of exploration from
  optimization. It is a hypothesis source until independently replicated here.
- [Karpathy's autoresearch
  program](https://github.com/karpathy/autoresearch/blob/master/program.md?plain=1)
  supplies the fixed evaluator, fixed short budget, append-only result ledger,
  keep-or-discard branch progression, hard timeout, and autonomous loop. This charter
  adds paired seeds, protected metrics, architecture gates, held-outs, and periodic
  simplification because Dissonance has multiple objectives and a much larger change
  surface than a single training file.

## 14. Start condition

The program begins only after a human approves this charter and requests the dedicated
research-director task. That task starts from the approved charter commit, creates the
batch branch and untracked artifact root, and executes E00. It may not skip directly to
an algorithm change or a 45-minute campaign.
