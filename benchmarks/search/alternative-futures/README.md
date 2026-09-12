# Alternative futures tranche — 2026-09-08

Status: active research; no implementation or breakthrough is qualified.

Start: 2026-09-08 20:26 UTC. Consolidation begins: 2026-09-09 06:56 UTC.
Hard end: 2026-09-09 08:26 UTC. The attached request is preserved in `request.txt`.
Base: `03b8c750f701bc8798f5109ef795265e3b683daa` (main, PR #268).
Branch: `codex/alternative-futures-20260908`.
Local worktree: `/private/tmp/harmony-alternative-futures-20260908`.
Private host root: `ms02:/root/harmony-alternative-futures-20260908`.
Prior artifacts under `/root/harmony-search-eval` are immutable inputs.

## Resource registration

ms02: Intel Ultra 9 285HX, 24 physical cores / no SMT; CPUs 0–7 advertise
5.5 GHz maximum and 8–23 advertise 4.7 GHz. 62 GiB RAM, 52 GiB initially
available; 501 GiB disk available. Initial process-name/resource inspection
shows no active research load. Preserve unrelated processes and artifacts.
At most four concurrent four-worker campaigns (32 GiB logical total, up to
8 GiB additional RSS allowance), leaving eight cores and at least 12 GiB for
verification, compilation, and the OS. Use fixed disjoint CPU sets and swap
paired arms across CPU sets; wall-time differences remain descriptive unless
measured in isolation. Keep cell outputs below 4 GiB and tranche outputs below
100 GiB; no licensed assets leave ms02. Use process-group watchdogs.

Metroid anchor: four workers, 8 GiB logical memory, 3M executions, 400M admitted
frames, 4096 actions, window/result slots 2/2, one-to-six suffixes. Initial
control is the frozen semantic selector/alphabet-only arm from build 012.
Any changed budget or diagnostic-start condition is reported separately.

## Hypotheses and decisions

1. Representation collisions: equal-count capability states share slots.
2. Destructive retention: useful alternatives disappear even with exact identity.
3. Selection starvation: admitted alternatives are not selected before removal.
4. Local exploration: selected states rarely advance with ordinary suffixes.

At most two implementation families may be active. First measure same-suffix
continuations of bounded replacement pairs and exposure before removal. Endpoint
inequality is not useful-future evidence; finite matching probes are not proof
of equivalence. Separate adapter identity changes from generic retention changes.

Known frozen evidence: Metroid semantic/alphabet-only seeds 3/4/5 reached Bombs,
Long Beam and Kraid area on 3/3, Ridley area on 1/3, no bosses. Quarter-share
continuation sped Bombs/Kraid entry but reduced beam/area breadth. MM2 historical
Wily 4 input replays, but current independent-stage panels do not qualify fresh
chained recovery. Issues #270/#271/#279 were read before choosing experiments.

## Expensive-command registration

Every experiment gets a record before launch with question, cheaper-evidence
gap, progress signal, wall bound, and decision branches. Record failed and
timed-out checks; do not repeat an unchanged expensive command.

### B00 — representative frozen baseline

Question: does frozen build 012 still run deterministically with current assets
and can the existing runner bound search/verification on this host?
Existing reports do not establish present executable/asset compatibility.
Run one Metroid 5,000-execution campaign with full replay, followed by a
100,000-execution anchor-profile fresh-genesis development seed 3 witness run.
Limits: 300 seconds per cell including a process-group watchdog allowance;
4 workers; 8 GiB logical memory. Signal: complete stream/checkpoint replay,
named witness replay, measured frames/RSS/disk and sensible work progression.
If qualification fails, diagnose before mechanism experiments. If it passes,
reuse the immutable build as control and begin collision/exposure instrumentation.
No solve or depth claim follows from B00.

## Checkpoints

- 20:26 UTC: tranche started; current main isolated; host and requested issues
  inspected. No fresh experiments yet. Initial literature pass capped at 45m.

Final delivery must separately report tranche finished, implementation verified,
and breakthrough achieved. Validation panels remain unchosen and untouched until
candidate/resource freeze; historical and development seeds are excluded.

### D01 — diagnostic neutrality and collision sampling

Question: can bounded pair/exposure observation preserve the exact baseline
stream while producing independently replayable collisions? Existing scalar
reports omit discarded inputs and cannot answer useful-future differences.
First run the focused generic observer test (10m watchdog), then a release
build (initial compilation, 20m watchdog). Run B00's 5k and 100k conditions
with audit enabled (5m per cell); require exact frozen stream hashes and full
small campaign replay. If changed, fix observer effects before long sampling.
If equal, qualify a 3M/400M anchor diagnostic at 4 workers/8 GiB, 90m total,
with frozen seed 3. Late sampling is necessary because prior evidence first
finds alternative equipment and areas after 1–3M executions. No diagnostic
seed is fresh validation. Probe work and sampled input storage stay separate.
The sampler retains 16 pairs per each of five categories, excludes competitors
whose snapshot is no longer cached (counted explicitly), and uses no campaign
RNG. Two 8192-action inputs per sample cap stored action payloads near 2.5 MiB;
metadata, serialization buffers, and reconstruction time also appear in RSS/I/O.
An empty capability category motivates finer diagnostics, not a proof of
behavioral equivalence. Discarded-only gains motivate identity/retention tests;
low exposure without useful paired differences motivates selection tests.

B00 passed: 5k jobs / 691,673 frames, full replay, stream
`c4860b4dae95ea88eeaf68882053e035eb0bf2689ef99a018003e32f1ae6c570`;
100k jobs / 13,629,183 frames, witness replay, stream
`bd2832ea030de2a6ba8b5247b71b560f52682dbbd4042a6847b115505d975cde`.
Cells took 21.1s / 96.6s, peak sampled RSS 171/274 MiB, disk 128/3.2 MiB.

MM2 stage-order provenance recovered from original session
`cd484718-ab48-58f8-8e35-7bcb3f22170b`, driver definition at JSONL line 1329,
extended at line 3110: metal, heat, air, wood, bubble, quick, flash, crash,
wily1, wily2, wily3, wily4 (then wily5/6 outside reach target).
Only this declared order is transferred; no historical gameplay inputs are
available to the fresh chain driver. The original source session's frozen hash
is recorded in `../results/transcript-remine-012.json`.

### P01 — equal-suffix qualification

Question: do discarded ordinary/resource competitors produce useful local
continuations the survivor misses? Endpoint inequality alone cannot answer.
Run the 100k diagnostic's 32 sampled pairs with 16 identical sampled suffixes
of 12 ordinary actions on both sides. Limit 5m, one CPU, two resident targets
and snapshots at a time, watchdog kills the probe process group. Require every
input to reconstruct its recorded competitor exactly. Report new equipment,
capacity/boss gains and living exits from the starting map separately, plus
actual frames and prefix reconstruction. Matching finite probes are inconclusive;
discarded-only local exits establish behavior loss, not deeper fresh success.
A signal qualifies a larger paired diagnostic on late samples (20m bound).

### C01 — fresh chained MM2 development baseline

`mm2_chain.py` can bound diagnostic comparisons with `--max-stages N` and
`--stage-frames F1 ... FN`. It records an early completed endpoint as
`stage_limit`, and a victory beyond a custom frame ceiling as
`victory_after_frame_limit`; neither qualifies Wily4. The default still
attempts the full historical order with one uniform `--frames` ceiling.
`--suffix` records the selected existing suffix profile explicitly. It never
accepts a supplied gameplay prefix. These driver controls do not alter the
search executable or its policies.

Question: can the current policy reproduce chained depth under the recovered
stage order, and where does a fresh chain fail? Independent-stage and historical
replay evidence cannot answer. Use new development seeds 20261001–20261003
(not validation), four workers/8 GiB, alphabet/energy-splice existing policies
registered per chain, same controller vocabulary and selector. First qualify
one 5k stage with full replay (5m). Then permit one attempt per stage, 1M
executions/120M admitted frames and 20m per stage, 90m per chain; no restarts
or imported inputs. Each successful stage carries only its searched victory
and adapter-owned menu transition. The driver records all failures and costs,
including repeated power-on prefix construction and two witness replays.
A failure identifies a stage-local bottleneck; a Wily4 reach must independently
replay twice and have all eight weapon bits. Neither outcome alone establishes
an algorithmic improvement. Broader chain budgets require a separate registration.

P01 completed: 32 pairs × 16 suffixes × 12 actions. All source endpoints
reconstructed. Discarded-only living map exits: 46/512; survivor-only: 30/512;
discarded survives while survivor dies: 18/512. No new equipment/capacity/boss
gains. This is local continuation loss, not fresh depth or global novelty.
Probe frames: 245,766 discarded / 242,472 survivor; prefix work: 378,462.
D01 100k audited stream exactly matches B00; full 5k replay also matches.
C01 5k Metal qualification passed full replay at 731,471 admitted frames.

### P02 — isolate resource tradeoffs

Freeze the currently available D01-long audit around 0.5M jobs; select its
resource-tradeoff category (equal equipment/capacity identity) only. Compare
64 ordinary identical suffixes of 24 actions per pair; 16 pairs maximum,
20m watchdog on CPU 16, no search feedback or extra archive storage. P01 had
no resource-tradeoff samples and cannot assess their preservation. Gains or
living exits available only to discarded tradeoffs qualify a bounded generic
resource-retention experiment. If both sides match, retain uncertainty and
inspect late capability collisions rather than automatically widening probes.
Preserve all paired outcomes, including survivor advantages. Longer suffixes
are diagnostic starts, explicitly separate from fresh validation.

P02 completed: all 16 resource-tradeoff sources replayed, 1024 paired suffixes.
Discarded-only living exits 156, survivor-only 42; only discarded survives 197.
No new pickups/bosses. Actual probe frames 707,878 / 489,897; prefix 898,376.
These repeated local counterexamples reject behavioral dominance by the
lexicographic representative rule on sampled states. They do not establish
that more retention improves global depth, nor do correlated suffixes constitute
1024 independent search trials. This qualifies family R: two resource extremes
under the same 8 GiB budget, with all other mechanisms held fixed.

Checkpoint before family R: run generic and NES library tests with 10m watchdogs
on CPUs 17–23. The purpose is to catch regressions in the diagnostic/evaluator
changes before modifying retention. Small actual-ROM stream/checkpoint replay
and both sampled-endpoint probes have already passed; incomplete tests block
this implementation checkpoint but do not erase the diagnostic evidence.


C01 first chain: Metal completed in 17,569 jobs / 2,960,855 admitted frames;
Heat setup failed before search. Diagnosis: `Mm2Target::apply` appends idle
award waits beyond the sampled hold, but the old driver's raw power-on export
concatenated sampled holds only. The physical tape therefore missed emulator
work. This is an export/chain compatibility defect, not a Heat search failure.
Correct the export by replaying the discovered input and recording each action's
actual hold plus implicit idle extension. Register C02 as a new fresh chain on
same development seed after bounded export/replay qualification; C01's costs
remain in the combined development ledger and are not hidden as a restart.
Generic/NES library checkpoint: 113/117 tests passed (1.34s / 15.45s).

### C02 — physical chain export repair

Check the corrected export on C01's already-discovered Metal input as a
**diagnostic fixture**. A direct power-on replay to Heat must retain Metal's
weapon and repeat twice. Bound: 5m including compilation excluded, CPU 16.
If matching, start a new fresh chain from power-on with seed 20261001 and the
same registered limits/policies; do not seed it with the diagnostic fixture.
If mismatching, inspect actual controller frames before another fresh attempt.

C02 refinement before launch: replay every generated next-stage bridge twice
before searching the next stage, and count those physical prefix/setup frames.
The first corrected fresh Metal result supplies this diagnostic naturally;
C02 will start from ordinary power-on and reuse no C01 gameplay input.

### R01 — bounded resource extremes, first candidate family

Preserve at most two representatives per slot: best under resource axis 0 then
1, and best under axis 1 then 0, with existing route cost and stable id breaking
ties. Metroid exposes health/missiles; MM2 exposes health/total weapon energy.
This is a deliberately small retention mechanism, not a claim that resource
ordering proves behavioral dominance or that two extremes preserve all tradeoffs.
Unsupported workloads retain their existing representative rule. Parent selector,
continuation mixture, suffix vocabulary, workers, total memory and work stay fixed.
The policy is separately recorded as `resource_extremes_2_v1`; omission preserves
historical behavior. More retained states consume the same archive byte budget.

First run focused extremes and full replay/pressure fixtures (10m watchdog),
then small actual-ROM campaign/checkpoint replay at 5k jobs for Metroid and MM2
(5m/cell), requiring actual alternative admissions. Run a 100k legacy control
against the frozen stream to detect unintended changes. On success, paired
Metroid development uses 500k jobs/70M frames, 4 workers/8 GiB and 20m/cell,
seeds 3/4/5, alphabet-only and semantic selection. Measure common milestones,
map coverage and work plus retention/exposure. A local retention win without
fresh improvement qualifies further depth testing, not default promotion.
A fresh regression with reduced exposure motivates separate selection diagnosis;
no automatic archive-size or budget increase.

R01 first focused compilation failed on missing test imports; no experiment
ran from that build. Corrected imports and made the audit skip competitions
where both endpoints survive. The focused retry passed both tests, including
actual alternative admission, replacement, eviction, continuation dispatch,
and exact campaign/report/checkpoint replay.

R01 actual-ROM qualification: both 5k campaigns reproduced full reports and
checkpoints. MM2 had 50 alternative admissions; Metroid had zero because seed 3
had not acquired missiles. The latter is compatibility evidence only. The
cross-workload generic mechanism is exercised by MM2 plus the synthetic
pressure/continuation fixture, so proceed to the registered bounded Metroid
pair and explicitly measure the first real alternative activity there. Do not
claim the 5k Metroid run exercised resource tradeoffs. The 100k legacy stream
from resource-005 matches frozen B00 exactly. Its 116s on E-cores is not directly
comparable with the earlier 97s on P-cores; attainment comparisons use admitted
work and paired placements, with wall throughput descriptive only.

Add a reporting-only final census of cached active endpoints for subsequent
builds: equipment/weapon union, capacity maxima, retained map coverage, and
missing-snapshot count. It scans once after workers join, retains no snapshots,
uses a temporary 32 KiB Metroid bitmap, and is explicitly a lower bound if
payloads were evicted. It is separate from the observed-anywhere accumulator
and independently verified single-trajectory witness. Qualification: generic
and NES library checks (10m), then unchanged-stream compatibility before use.

Resource-005 development pair seed 3 started after qualification, control on
CPUs 4–7 and candidate on 12–15. Both run the same attested binary; resource
policy is the only search change. Resource-006 adds the final census and clearer
chain cost/status labels (including avoiding a redundant final bridge replay);
its NES library 117 tests, generic 115 + 1 interface tests, evaluator 1 test,
Python runner 22 tests, and dependency check passed. No unsafe code changed.

P02 per-pair reanalysis is retained in `results/p02-per-pair.json`. Discarded
representatives do not uniformly win: several survivors have better exit rates
per actual emulated frame. Paired identical allowances stop early on death,
so actual frame totals differ. These finite counterexamples disprove universal
behavioral dominance, but they do not prove an equal-cost global search gain.

Inspection against main confirms `in_window_ever` is an existing selector vector,
not newly allocated by the audit. The observer reuses it and adds only fixed
counters. Resource-008's new diagnostic-memory field mistakenly includes that
existing vector capacity; this is conservative overreporting, not extra memory
or a search change. Correct the field to fixed observer storage at the next
source checkpoint. The 2.5 MiB workload action-reservoir bound remains explicit.
A preparation script syntax error made resource-007 a duplicate of 006; it has
no experimental role. Resource-008's full source bundle was frozen and verified
against its build hash before new manifests were generated.

### C03 — fresh chained MM2 retention transfer

C02 has cleared all eight Robot Masters from its own fresh inputs and verified
every bridge twice. MM2's small resource-extremes campaign already exercised
50 alternative admissions and exact full replay; Metroid seed 3 was mixed.
Run one fresh resource-008 chain on the same development seed 20261001 and
identical stage order, selector, energy-splice vocabulary, 4 workers/8 GiB,
1M executions/120M admitted frames, 20m per stage and 90m total. Start on CPUs
0–3 after D01 completes; C02 uses 8–11. Compare attainment and admitted/prefix
work, not unmatched core-class throughput. No C02 input enters C03. A deeper
or cheaper chain qualifies repeated development; a failure identifies a
transfer regression and is retained. Neither a lone successful chain nor a
replayed historical tape qualifies the validation threshold.

D01 anchor completed: 3M jobs / 382,101,298 frames, observed 155 map cells,
Bombs, Long Beam, energy tank and Kraid area, no boss. First named milestones
and work match the frozen semantic seed-3 control. Audit census over 8,161,836
eligible same-slot competitions found 0 equipment and 0 capacity collisions,
686,536 resource tradeoffs; no incumbent snapshots were missing. Of 946,740
removed representatives, 245,849 (25.97%) had never been selected. This narrows
identity collisions as an explanation for this seed, not for all future states.
Independent witness replays preserve the attained capabilities on one route.
Compact provenance and evidence are in `results/d01-anchor.json`.

At the next implementation checkpoint, make optional resource retention fall
back to ordinary retention if any competitor lacks resource axes. This guards
the generic optional-key boundary without changing Metroid/MM2 (always present)
or other current workloads (always absent). Qualify the mixed-availability
fixture plus the generic and NES contracts under 10m watchdogs before commit.

Resource checkpoint: 116 generic unit tests plus 1 interface test, 117 NES
library tests plus 1 evaluator test, 22 Python runner tests, dependency boundaries,
and diff whitespace checks pass. The optional-policy generic fixture exercised
real alternatives, replacement, eviction, continuations and exact replay. The
actual-ROM resource-008 small streams and legacy 100k stream exactly match the
previously qualified counterparts. The mechanism remains experimental: early
Metroid seed 3 is mixed; seed 4 misses the control energy tank and covers 79
versus 82 map cells. C03 reaches the end of Heat in about 52M stage-search frames
versus C02's 105M, with different fresh prefixes from their own Metal searches;
that is an end-to-end chain comparison, not an isolated same-start Heat effect.
