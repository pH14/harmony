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

### R02 — retention at the registered depth anchor

After the R01 three-seed short panel, run resource retention on development
seed 3 at the existing 3M execution / 400M frame / 4-worker / 8 GiB anchor,
alphabet-only and semantic selector unchanged. The short panel exercises the
mechanism but cannot assess the control's first Bombs discovery at 2.847M
jobs. This is the registered anchor, not a larger exploratory budget. Bound:
5280s search + 120s finish (90m); no checkpoint or historical input as origin.
Use the completed D01 and matching frozen 012 control as comparator. Compare
named progress at common admitted-work boundaries, observed/retained/single-
trajectory capabilities, exposure, memory, and actual frames. Better late
attainment qualifies replication on reused seeds 4/5; a late regression or
unchanged depth with weak exposure motivates a separate selector/continuation
comparison, not a budget increase. One seed cannot qualify a breakthrough.

New experiment manifests now live outside hashed source directories and refuse
existing names. Prior manifests remain preserved. Resource-009's full 488-file
source bundle exactly matches attestation
`f3c71d8ff926e1415310e1913a91bb155ec9f44b6fbdd550f399846881028296`;
binary SHA-256
`3faa91d35484802921cd1b5d8f790c4229fed5cc1bd8688442577296f387a32a`.
Only the optional-resource availability guard and corrected diagnostic-storage
label differ from the preceding qualified resource mechanism. Run the bounded
actual-ROM smoke again before using this binary at depth; no new gameplay
policy or budget is introduced by that qualification.

R01 short panel complete: observed map cells control/resource are 87/81,
82/79, 80/88 for seeds 3/4/5. Energy tank observed 3/3 control versus 2/3
resource; no deep pickups or bosses at this horizon. This does not establish
an improvement. R02 now tests the late behavior for which this horizon is
inadequate, without increasing the registered work or memory limit.

### E01 — retention with campaign-learned continuations

Second and final active candidate family: combine the same bounded retention
with the existing quarter-share `alphabet_continuation_v1` policy. Frozen 012
shows earlier Bombs/Kraid area under that policy but reduced beam breadth;
P02 shows discarded resource alternatives can supply missing local exits.
Question: does retention preserve useful breadth under learned continuation,
or merely dilute its faster exploration? No new vocabulary or controller tape
is introduced; all continuations are learned in that campaign. First qualify
5k Metroid full replay (5m), then one matched seed-3 500k/70M/4-worker/8 GiB
comparison against continuation-only, 20m/cell, CPUs 8–11 and 12–15. Generic
combined retention/continuation/eviction replay already passed. Inspect named
capabilities, common-frame map coverage and exposure. A consistent local gain
qualifies a full-depth combined probe; a clear regression redirects toward
exposure/local-suffix diagnosis instead of automatically widening budgets.

C02 ended at Wily 1's wall limit with admitted stage work below 120M frames.
That is a censored stage result, not an exhausted fixed-work failure. Preserve
all eight successful Robot Masters, all prefix/replay costs, and this failed
stage. Any comparison with C03 must use common admitted work or explicitly
state the censoring; further fixed-work qualification gets its own registration.

### L01 — diagnostic local search from a discovered deep endpoint

D01 first enters Kraid area at job 2,937,978, leaving only about 62k jobs in
the anchor. Question: can ordinary local search advance from that reproducible
endpoint, or is local action generation itself ineffective? Use D01's own
first-Kraid-area input, verified twice from ordinary genesis, as an explicitly
**diagnostic snapshot root**, never a fresh validation origin. Preserve the
4096 total-action horizon by subtracting the source prefix length from the
local allowance. No supplied route, hand-picked control sequence, or reward
change is permitted. Implement only a bounded diagnostic executable using the
existing snapshot-root campaign API; no searcher or game mechanics change.

First qualify 5k full campaign/checkpoint replay from that root (5m) and replay
the composed full witness twice from ordinary genesis. Then compare ordinary
`one_to_six` against the existing generic bounded full-hold suffix
`one_to_six_within_3_longest_actions_full_hold`, each 100k jobs/20M admitted
frames, 4 workers/8 GiB, semantic selector, alphabet-only, development seed 3,
20m/cell. Charge source verification, preparation, search and composed-witness
replay separately. New bosses or capabilities qualify a fresh-search follow-up;
only differing endpoints do not. If both fail without useful separation, stop
this probe family and reconsider exposure rather than increasing its budget.

E01 ended with 59 versus 55 map cells (continuation-only versus combined),
no energy tank or deeper capability in either, and 50,777 versus 56,297 removals
before selection. Combined retention reached Norfair earlier but did not show
the consistent local gain required by this registration. Park family E; do not
claim its full-depth behavior disproven. Active candidate families are now R
and the separately isolated selection hypothesis S below.

### S01 — isolate MM2 location ranking from mechanical progress

The C02 Wily-1 witness is only 240 actions long, so the 4096 horizon is not a
supported explanation for its plateau. Its historical frontier selector ranks
opaque location identities, while the current MM2 adapter already supplies a
separate progress relation using boss clears/damage (documented in its README).
Question: does choosing on that declared progress relation escape the same
stage-entry bottleneck at equal admitted work? Use the C02-discovered Crash
bridge as a **diagnostic stage-entry prefix**, never an external fresh-chain
input. Run semantic selection only, ordinary representative retention, same
seed 20261001, energy-splice:6, 4 workers/8 GiB, 500k jobs/50M frames, 4096
actions, window/result slots 2/2, 20m watchdog on CPUs 8–11. Compare the completed
C02 stage's recorded progress at the same 50M-frame boundary. No new game
reward, route, weapon advice, or decoder change. Reaching a boss/stage transition
that control misses qualifies a new fully fresh selection-only chain; matching
failure does not qualify a larger budget or a selector parameter sweep.

L00's first launcher failed before emulation because ms02 lacks `/usr/bin/time`.
Preserve the exit-127 log. Use Python's `wait4` process resource accounting
instead; the corrected launcher uses a new qualification name and preserves
the same input, limits, process-group watchdog and source-endpoint assertion.

## Two-hour checkpoint — 22:25 UTC

Completed/failed benchmark cells: 44 (43 complete executions, one infrastructure
error; completion does not mean solved). These cells consumed 10,493,514 jobs,
1,597,809,747 admitted frames and 39,157.30 process-tree CPU seconds (10.88 CPU
hours, including their setup/verification). R02 is ongoing at 2,070,400 jobs /
273,341,186 frames; diagnostic local/probe work and compiler costs are additional,
with their own records. No untouched validation seeds have been inspected.

Facts: D01 reproduces the anchor's work and milestones and sees no equipment or
capacity collisions across 8.16M eligible competitions on seed 3. Finite paired
suffixes disprove universal behavioral dominance by resource preference, but
small bounded retention does not reliably improve fresh progress. R01's early
50M-frame coverage gains reverse on two seeds by 500k jobs. E01 has no qualifying
local gain and is parked. R02 has reached Long Beam at 2.062M jobs, later than
control's 1.762M, but remains active until its registered stopping condition.

Both fresh MM2 chains acquired all eight weapons. C02 stopped in Wily 1 at the
wall limit after 98,421,774 stage frames; C03 stopped there after 118,402,491.
Their total admitted chain frames are 286,628,099 and 323,395,219 respectively.
The early Heat advantage did not survive the complete Robot Master sequence.
Neither reached Wily 4. S01 semantic-selection-only, from C02's same stage-entry
prefix, exhausted 50M frames without reaching a boss. Changing location ranking
alone is therefore insufficient at that work on this diagnostic start.

L00 reproduced D01's exact source snapshot, full local campaign and checkpoint,
and composed ordinary-genesis witnesses. L01 ordinary and bounded-hold local
search both increased missile capacity to 15 without a boss. The ordinary
witness reports health 9800 in tenths with one energy tank; independent replay
repeats it, but its mechanical meaning needs inspection before treating it as
useful resource improvement. Local snapshot-root runs reset archive history and
learned continuations, so they are not continuations of D01's full archive.

Candidate status: R remains experimental; E is parked; S has no qualified fresh
candidate. No defaults change. Next decisions: finish R02; inspect raw state and
native endpoint rendering for the unusual local witness and shared MM2 plateau;
redesign the failed probes based on those observations rather than increasing
work automatically. Preserve the existing SMB reference and run Nova/STB checks
at the next available isolated checkpoint. Consolidation remains 06:56 UTC;
hard stop remains 08:26 UTC. This checkpoint records research progress, not a
finished tranche, complete validation, or a breakthrough.

## I01 preregistration — raw endpoint inspection

Question: does L01 ordinary's decoded health 9800 reflect the physical emulator
state, and what is visible at the repeated MM2 Wily 1 endpoint? Existing witness
replay establishes reproducibility but uses the same adapter decoder. A separate
raw-machine replay of the exact discovered input, with per-frame RAM and one
native endpoint image, can expose a transient or decoder mismatch without any
new search or controller advice. Add a bounded diagnostic CLI (no policy change),
compile under the existing 20-minute initial-build watchdog, then allow five
minutes for each replay, at most 20k actions / two million physical frames and
64 anomalous health transitions. Run L01 ordinary and C02 Wily 1 only initially.
Require raw endpoint agreement with the adapter (except explicitly derived MM2
fields); retain raw RAM, native image, input/core/ROM hashes and frame costs.
A mismatch requires adapter investigation before resource claims. Agreement
establishes a native state effect but not usefulness; use the visible endpoint
only to diagnose measurement/local-exploration boundaries, never to supply a
route or controller sequence. No fresh validation evidence is produced.

I01 Metroid raw replay matches all adapter fields. Health jumps 37 → 9800 on
the final physical frame (98431), bytes $106=00, $107=98; this is native state,
not a replay mismatch. Add a 120-frame neutral-input observation-only tail to
identify transient death/resource semantics, charged separately and never used
as a fresh search start. MM2 inspection failed after writing its image because
it requested save RAM on a cartridge without that region; preserve failure and
correct the diagnostic to read cartridge RAM only where the decoder uses it.
Build 011 was unused after correcting a diagnostic mode-byte label; build 012
ran I01. Freeze corrected build 013 and retry under the same five-minute bound,
with new names. If the high health immediately resolves to death, investigate
terminal observation timing before further retention tuning.

R02 completed 3M jobs / 392,442,771 frames. It admitted 599,473 alternatives,
observed 107 map cells, reached Long Beam late, and missed Bombs/Kraid area.
Control observed 155 cells and reached both. No boss in either. This policy is
not supported as an improvement at the fixed anchor; retain it as an explicit
experimental ablation and park family R.

## G01 preregistration — candidate checkpoint regressions

Question: do the generic retention changes preserve the qualified SMB reference
and companion workload replay? Small Metroid/MM2 checks do not cover these
adapters. Reuse the frozen five-seed SMB evidence and repeat representative seed
20260910 under its exact 24-worker/2-GiB/600k-job/120M-frame reference condition,
ordinary retention. Allocate the host exclusively to this cell now R02 is done;
retain 600s search plus 120s finishing and a 750s process-group watchdog. This is
an explicit temporary CPU-allocation exception (all 24 workers) for reference
compatibility, with ample memory headroom and no concurrent campaign. Require
solved and compare the frozen reference's deterministic work/stream evidence.
Then run Nova level 1, Nova whole-game and STB Hard 500-job full campaign replay
qualification at 2 workers/512 MiB, once with ordinary retention and once with
resource-extremes (their adapters supply no resource axes, so fallback should
preserve streams). Each cell has 120s search/60s finish and 210s outer watchdog;
whole suite watchdog 1260s. A failure blocks general compatibility claims and
requires targeted diagnosis; passing establishes checkpoint compatibility, not
new whole-game performance or a default-policy recommendation.

## T01 preregistration — terminal observation correction

I02's raw state equals the adapter endpoint in both games. Metroid's 9800 health
becomes zero after one neutral frame and stays zero for all 120 observed frames;
mode changes from play to death at offset 89. This is a transient damage-underflow
state, not usable health. The pinned disassembly's Bank07 SubtractHealth
($CED7–$CEF6) stores the BCD subtraction before checking borrow and zeroing health;
a video-frame boundary can expose that intermediate RAM. Tank pickup caps the
count at six ($DC03), and normal full health is 6999 tenths. Source:
https://github.com/nmikstas/metroid-disassembly/blob/4270d57f9468daebdeea485686e31e26218a780c/Source_Files/Bank07.asm
No route or controller advice is drawn from this source.

Question: do transient impossible-health states poison same-slot retention?
Family T is a workload-terminal correction, isolated from archive identity,
retention, selector and local actions. R and E are parked. Add an explicit
`death_or_bcd_underflow_or_ending_v3` policy that treats the BCD high-byte sign
range (decoded health >=8000) as terminal, alongside the existing zero predicate.
Keep legacy v2 available and the default for historical byte-compatible replay;
record/reject mismatched terminal policy headers. Do not clamp healthy resource
values, rank items, or add game-specific generic-search logic. The >=8000 range
is deliberately separate from ordinary maximum-health capping; no broader
invalid-state inference is claimed.

Before fresh development: focused tests under 10m for the underflow frame,
legitimate full health, ordinary damage and legacy/corrected policy separation;
replay I02's witness with both policies and one-frame continuations from all
256 controller masks (<=256 additional frames plus restoration, <=5m). Require
all physical continuations to enter zero health and corrected admission to
reject the endpoint while legacy retains it. Then a <=5k-job full campaign/root
checkpoint replay from the earlier D01 development input (<=5m) must exercise
this terminal change, with ordinary-genesis witness replay twice. A failed
causal check blocks a larger campaign. Passing supports a 12M-frame paired local
search at fixed source and a fresh development comparison; it does not establish
that the correction improves depth or explains the whole anchor failure.

T01 focused checks passed: 120 NES library tests plus the nes-eval unit test,
including preservation of raw underflow health, valid-health acceptance, strict
policy identifiers and rejection across replay contexts (15.38s compilation,
25.52s tests). To make the <=5k full replay exercise the observed defect, use
L01's own witness with its final action removed as a second bounded diagnostic
origin. This is automatic truncation at the already-identified underflow action,
not a hand-authored controller or fresh start. Compare legacy/corrected from
that identical live prefix before any larger local or fresh run. Retain source
hash and two ordinary-genesis replays; require actual differing admissions or
deaths and full report/checkpoint equality on replay in both arms.

T01 all-mask causal check passed: each of 256 one-frame physical continuations
has zero health; legacy labels the exact 9800 endpoint alive and v3 labels it
dead, without changing any mechanical field. Both 5k local campaign/checkpoint
replays passed but their deterministic work/admissions are identical (587,447
frames, 2,924 retained, 1,848 deaths). They did not exercise the terminal change.
Refine the qualification origin to the same discovered tape truncated by exactly
one physical frame (reduce its last hold from 8 to 7), instead of removing the
whole last action. This tests the known frame-boundary defect directly under
ordinary random suffixes, with the same 5k/2M/240s limits and new immutable names.
This fixture is not a development improvement or a validation seed.

G01 SMB exactly matches frozen reference seed 20260910: 301,688 jobs /
42,017,148 frames, stream 33112e2a5dc91ed95635cdff4c162c901392d0a3b24be57f26e67b57b0ef15b6,
solved. Nova level 1/full and STB Hard full replays pass in both retention arms;
normalized reports differ only by the explicit slot_retention header and stream
hash. Their unchanged work is 80,586 / 80,586 / 83,148 frames respectively.
No new whole-game companion success is claimed from these 500-job checks.

T01b full replay also passes in both arms with identical work: 207,761 frames,
only the root retained, 5,000 deaths. Reading the action vocabulary explains why:
its shortest action is two frames, so starting one frame before underflow skips
past that intermediate value before any action endpoint. Redesign the fixture
around this executable constraint: truncate the discovered final hold by two
frames (8→6). The ordinary two-frame draws can then end on the known underflow
frame. Keep the same bounds; this is a frame-alignment correction, not a budget
increase. If this third fixture still has no differing admission, stop this
qualification approach and use the direct execution/admission contract evidence
without claiming full-campaign anomaly coverage.

T01c passes actual-mechanism qualification. Both 5k full campaign/checkpoint
replays agree exactly with their live runs. Legacy retains 3 states / 4,983
deaths / 209,720 frames; corrected retains 2 / 4,986 / 209,750. The causal
all-controller probe establishes the rejected branch's terminal future. The
fixture is intentionally almost terminal and does not measure useful depth.

## T02 preregistration — equal-frame local and fresh development

Question: after preventing false high-health admission, does ordinary exploration
preserve useful futures or progress more deeply? T01 proves the defect and replay
mechanism but cannot answer search efficacy. Use the unchanged D01 first Kraid
area input (own development discovery) for paired legacy/v3 local searches:
4 workers, 8 GiB, alphabet-only, one_to_six, semantic selector, 8M admitted frames,
100k execution safety cap, 1080s search plus verification within a 20m cell.
Eight million is below both prior L01 frame totals, so the frame limit should
bind; if executions or wall bind first, label censoring instead of extending.
Keep ordinary retention. Compare boss/capability attainments, map coverage and
retained/live exposure; replay composed witnesses twice from ordinary genesis.

Also qualify terminal-014's legacy path against the frozen 100k seed-3 stream
under the <=5m legacy condition. If it matches, run fresh v3 development seeds
3,4,5 at the unchanged R01 500k/70M/4-worker/8-GiB condition; compare against the
already frozen ordinary-retention controls. Same selector/vocabulary/mixture,
no retained inputs. Each fresh cell has 1080s search +120s finish and a 1230s
outer process-group watchdog. Allocate local arms CPUs 4–7 and 12–15; legacy
compatibility CPUs 0–3, then fresh seed3 there. At most four 4-worker campaigns
and 32 GiB logical search memory at once. Swap P/E class for subsequent seeds.
Timing is descriptive unless CPU class matches. If v3 changes no short-search
work, use the known late underflow witness to justify one fixed-anchor long
probe; a short run cannot settle a late defect. If short work changes, require
repeatable capability/exposure benefit or explicitly register a depth-specific
question before going long. No resource-retention combination yet.

T02 local arms both stop at 8,001,091 frames / 66,023 jobs, with identical
40-cell coverage and no new capability. This is earlier than L01's known
underflow-producing search, so it does not test downstream effect. Preserve
this limit explicitly. Add constant-size, reporting-only counters for observed
underflow action endpoints, candidate eligibility and first execution, plus
maximum endpoint health and final cached underflow occupancy. These counters
must not change stream/report bytes or add snapshots/reconstructions.
The metadata records their fixed memory separately.

To cross the already demonstrated late event, register T02b at 12M frames with
a 150k-job safety cap (raise only the bounded diagnostic CLI's cap from 100k,
keep full replay <=5k). Same D01 local origin and policies, <=20m per cell.
The reason for 12M is L01's directly observed anomaly by 11,827,246 frames, not
an unobserved hope of progress. Compare both arms at the common frame boundary;
if either lacks an underflow endpoint, the local efficacy question remains
unanswered and do not increase it again. First recheck the vocabulary-aligned
5k fixture and legacy 100k hash with the new reporting build, then run T02b.
Fresh T02 cells already running keep their immutable build and original scope.

Changed-target Clippy found diagnostic wall-clock calls outside the existing
explicit telemetry allowance and a collapsible conditional. Move only those
clock reads into documented diagnostic helpers and simplify the conditional;
these are reporting wrappers, not a campaign semantic change. Preserve the
failed check log and rerun changed-target Clippy. Focused tests already passed.
