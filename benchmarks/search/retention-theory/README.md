# Retention theory and msr1 experiments

Status: active research. No breakthrough is qualified.

Start: 2026-09-08 22:16:18 UTC. Theory deadline: September 9 00:16 UTC.
Consolidation: 08:46 UTC. Hard tranche end: 10:16 UTC.
Worktree: `/private/tmp/harmony-retention-theory-msr1-20260908`.
Branch: `codex/retention-theory-msr1-20260908`.
Private host root: `msr1:/root/harmony-retention-theory-20260908`.

The research baseline is commit `8e5ae6830775d886d6f486d81115b3f655e1128d`,
which adds diagnostics and experimental two-extreme retention on top of PR #268
(`03b8c750`). That committed work belongs to the separate ms02 research effort.
We reuse it without modifying its worktree, runs, or uncommitted local-search
experiment. Resource comparisons use the same frozen binary for both arms;
key comparisons freeze separate feature builds from identical source.
Its optional retention policy is disabled in controls.

msr1 has 12 ARM cores (four A520 and eight A720), 54 GiB RAM and about 792 GiB
free disk. At most two four-worker campaigns, each 8 GiB logical archive, may
run concurrently, with disjoint CPU sets 0–3 and 8–11. Four little cores remain
for compilation/verification; compilation may use idle big cores. Core classes
have different clocks, so matched-work results are primary. Paired placements
must be swapped before interpreting throughput. Bound total output to 80 GiB,
each cell to 4 GiB, and each experiment with the runner's process-group watchdog.

The user explicitly approved transferring the existing Metroid asset from ms02
at the first checkpoint. The copy to msr1 has the expected SHA-256
`e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6`.
MM2 and SMB assets already on msr1 also have the expected hashes.

## Questions and evidence gates

The objective is useful future discovery per actual work under a fixed memory
budget. Archive size, resource fronts, and local exits are diagnostic quantities,
not substitutes for fresh deep attainment.

1. Does a proposed abstraction preserve task-relevant continuation traces?
2. Which retention decisions remove distinguishing futures?
3. Does allocation reach retained alternatives before they disappear?
4. Do improvements survive fresh searches and independent replay?

The mathematical contract and executable finite counterexamples are developed
in `theory.md` and the searcher's abstraction fixtures. They must state exactly
which assumptions are checked. Finite probes may refute equivalence; matching
finite probes cannot establish it for the NES.

Existing development evidence: D01's 3M-job Metroid seed 3 found 686,536 resource
tradeoff competitions but no equipment/capacity collisions among 8,161,836
eligible competitions. Of 946,740 removed representatives, 245,849 had never
been selected. P02's 16 pairs and 64 identical suffixes per pair found 156
discarded-only versus 42 survivor-only living map exits. Actual frames differ
because terminal branches stop early. This rejects universal behavioral
dominance, but does not establish a fixed-cost campaign benefit. The existing
two-extreme development arm has mixed results. Source evidence is linked from
`../alternative-futures/results`.

Development seeds: historical 3, 4, 5 plus 20261101–20261103 for this effort.
Do not use the other effort's 20261001-series as unseen validation. Validation
seeds will be frozen only after selecting a mechanism and budgets. A breakthrough
requires matched-control improvement and at least 3/10 fresh Metroid boss seeds
or 3/5 fresh MM2 Wily 4 chains, with both games evaluated. Report failures and
timeouts, and separate verification of implementation from breakthrough claims.

`validation-protocol.md` distinguishes the operational repeatability thresholds
from exact paired evidence and defines how untouched panels, censoring, and
multiple endpoints will be reported. Development gates allocate compute; they
do not prove eventual success or impossibility.

## Experiment ledger

### B01: ARM baseline qualification

Question: do current source and assets execute and replay correctly on msr1?
The x86 evidence cannot establish ARM compatibility. Build the pinned QuickNES
revision and baseline source with Rust 1.97.0, bounded at 10 and 20 minutes.
Build failures are infrastructure failures, not negative scientific evidence.
Source identity must remain unchanged during compilation.

Run SMB and MM2 Metal, 5,000 jobs each, four workers, 512 MiB, window/result
slots 2/2, ordinary existing policies, full campaign/checkpoint and repeated
witness replay. Limit each cell to 240 seconds plus 60 seconds finishing time.
Run one cell at a time on CPUs 0–3. If successful, retain immutable attestations
and qualify larger mechanism tests. If unsuccessful, diagnose before expanding.
No deep-search claim follows from qualification.

An initial source transfer raced directory creation and failed before copying;
it was retried after creation succeeded. The licensed ROM transfer was rejected
before execution; it succeeded after the user's subsequent explicit approval.
Neither infrastructure event consumed search work.

B01 passed: both 5,000-job cells completed full campaign/checkpoint and witness
verification. SMB admitted 689,658 frames; MM2 Metal admitted 564,972 frames.
The four exact-model tests passed locally in 0.01s after an 18.5s first build.
They show actual production admission losing a distinguishing future under
count aliasing, and losing a monotone two-resource threshold exit under the
two-extreme rule in all six arrival orders. Partition refinement restores the
former model's task future; it is not an NES fix by itself.

### R02: two-representative resource threshold coverage

The exact threshold counterexample and existing P02 continuation losses qualify
one new retention family: choose up to two states maximizing the number of
nonnegative integer resource thresholds jointly satisfied by either state.
For axes h,m, one state covers `(h+1)*(m+1)` thresholds, including zero-axis
requirements. Choose the exact best subset among the current (at most two)
representatives plus the candidate. Prefer fewer states on equal coverage,
then existing route cost and stable id. No extra probe execution or snapshot
capacity is introduced. The origin and integer units are part of this policy.

This is local resource coverage, not behavioral dominance or a guarantee about
the full stream of discarded states. It assumes uniform threshold importance
only as a selection surrogate. It has no global greedy approximation claim.
Qualify exact arithmetic, the middle-resource counterexample, missing-axis
fallback, deterministic tie handling, pressure/continuation replay, and old
policy compatibility before running actual-ROM trials. Bound tests at 10m and
each release build at 20m. If these pass, register a small three-arm development
comparison (legacy, extremes, coverage) with unchanged work and memory. If the
proxy does not improve useful retention or fresh attainment, reject it without
expanding run length merely to seek a success.

R02 implementation checkpoint: 124 generic unit tests and one interface test
pass, including coverage retention under pressure with actual continuations and
exact report/checkpoint replay. All 117 NES library tests pass. Strict generic
Clippy initially found an iterator style issue and an inherited test idiom;
both were corrected and Clippy plus the generic suite pass. No unsafe code changed.

### Q02: actual-ROM coverage and compatibility

Use the R02 frozen build and B01's exact conditions/seed, first with no optional
retention. Require both B01 stream hashes to match. Run MM2 under both optional
policies and SMB under coverage, 5,000 jobs each, full replay, the same 240+60s
per-cell bound. Require actual alternative admissions on the MM2 candidate;
SMB must retain legacy decisions because it supplies no resource axes. An empty
alternative count is compatibility evidence only, not mechanism qualification.
Only after passing Q02 may C01 begin.

Qualification correction: an explicitly requested policy necessarily changes
the stream header's `slot_retention` tag even on unsupported workloads. The
initial Q02 driver incorrectly expected the entire SMB coverage hash to match.
Its recheck mode instead requires exactly that one header difference and
byte-identical remaining records. This corrects the acceptance test without
rerunning completed campaigns or relaxing any decision/replay comparison.

### C01: fresh MM2 coverage transfer development

After Q02, use untouched development seed 20261101 in two fresh chains: legacy
and coverage, same frozen binary and existing chain driver, no imported input.
Order: Metal, Heat, Air, Wood, Bubble, Quick, Flash, Crash, Wily 1–3, then verify
Wily 4 entry with all eight weapon bits. Every carried prefix comes exclusively
from that chain and is replayed twice. Fixed operator stage order is declared;
this is not unrestricted whole-game planning.

Each stage: four workers, 8 GiB logical memory, 1M jobs/120M admitted frames,
4096 actions, window/result slots 2/2, energy-splice:6, legacy frontier-cheapest
selector, at most 1,200 seconds plus 120s finishing. Chain hard limit 5,400s,
outer process-group bound 5,460s. Place legacy on 0–3 and coverage on 8–11.
The question is whether threshold coverage improves fresh chained attainment
or the cost of reaching common milestones. Record all failed stages and prefix
work. If the candidate fails earlier, investigate that stage without restarting
the chain under the same identity. If both fail at the same bottleneck, inspect
retention/exposure evidence before increasing work. A lone deep result cannot
qualify the breakthrough; repeated fresh validation remains a separate gate.

23:10 UTC checkpoint: Q02 passed, including actual MM2 alternatives and SMB
byte-identical records after its explicit policy-header difference. The original
driver failed its incorrect full-hash assertion after all five cells completed;
the corrected checker qualified those same outputs without rerunning search.
Coverage and extremes make the same decisions in this small MM2 case, so it
does not establish an advantage of the new objective. Both fresh C01 chains
cleared Metal and replayed their bridges twice: legacy 6,786,404 frames/43,624
jobs, coverage 5,260,416 frames/30,149 jobs. This is one development comparison.

The theory phase is complete within one hour: seven executable abstraction
fixtures cover both favorable and adverse cases, including a reverse example
where coverage forgets a later useful complement and extremes retain it. A
finite seed-panel fixture also shows the production selector losing one-attempt
goal discovery probability after retaining an additional distinct future. The
candidate is a testable resource surrogate, not a universal dominance rule.

### A01: distinguish coverage from a second resource representative

Existing C01 first-stage searches share ordinary power-on genesis, while their
Heat prefixes differ. Run one additional fresh Metal stage on development seed
20261101 under `resource_extremes_2_v1`, using the same frozen candidate binary,
4 workers/8 GiB, 1M jobs/120M frames, 4096 actions, identical vocabulary,
selector, window and result slots. Limit this diagnostic to 600s search plus
120s finish, 750s process-tree hard limit. A timeout is censored and cannot be
treated as a matched-work loss. Compare victory work and input hashes with the
two completed C01 Metal stages. Matching coverage would attribute that Metal
improvement to their shared behavior, not the coverage objective.

For this one bounded ablation, temporarily allow a third campaign on little
cores 4–7 while C01 uses 0–3 and 8–11. Total logical archive allocation is at
most 24 GiB; no compilation or other new CPU-heavy work runs concurrently.
The dedicated host has 54 GiB RAM. All timing remains descriptive. Run this
new job in a systemd control group with a hard memory/runtime limit so child
sessions cannot outlive the driver. Return to two campaigns when it completes.

A01 passed: extremes exactly matches coverage's 30,149 jobs, 5,260,416 frames,
victory input and next-stage prefix. Deterministic campaign reports differ only
in policy, requested wall limit and stream hash. Therefore the Metal saving
over legacy does not validate the new coverage objective.

### A02: same-start Heat diagnostic at 100k jobs

Continue from A01's own discovered, exported Metal victory under extremes;
its prefix hash exactly matches C01 coverage's Heat prefix. This is a diagnostic
continuation, not a fresh validation chain. Use that fixed origin, seed 20261101,
4 workers/8 GiB and all C01 vocabulary/selector/window settings, with 100,000
jobs and 20M frames, 600s plus 120s finish, a 750s systemd process-tree bound,
on little cores 4–7. Allow the same temporary third-campaign allocation as A01.

Compare the completed diagnostic with C01 coverage's recorded 100k progress
boundary (or the nearest strictly earlier common boundary if that record is
absent). Execution/frame ceilings must not be treated as equal if a wall stop
occurs first. The question is whether the new objective actually changes
retention and common-origin progress, beyond the shared two-state mechanism.
Do not infer whole-chain improvement from this diagnostic. No old solution
input or hand-authored gameplay enters either origin.

P02 retrospective check: the single-state threshold-volume preference has only
3 strict agreements with the better observed living-exit rate per actual frame
among 15 non-tied pairs (one additional pair is tied).
Only 3/16 pairs share exact pixel position and 12/16 share raw pose. This does
not support treating resource volume alone as a predictor of these local exits;
the data contain no boss gains and do not test selecting two states. Numeric
endpoints were extracted from the frozen audit with its matching SHA-256, with
no ROM bytes or gameplay inputs transferred. Keep this adverse association
beside any favorable fresh result.

Reproduce the numeric analysis with `python3 analyze_p02.py`. Fifteen pairs
have at least one distinguishing local-exit or survival probe. An 8-pixel
position partition would separate 9 of those pairs; raw pose separates 4;
their combination separates 11 and leaves 4 merged. This identifies a concrete
candidate abstraction refinement, but does not measure its archive growth or
prove fresh-search improvement. The current production key and all experiments
remain unchanged. Metroid emulator qualification still requires the pending
asset-transfer approval.

Review of the finite checker found a shortest-witness corner case when a later
cost difference was encountered before an already-shorter state-label difference.
Checking successor labels when visiting each edge fixes it; the new regression
and all eight abstraction fixtures pass. This changes test scaffolding only,
not the frozen candidate executable.

C01 ended at Heat's registered 20-minute wall limit in both arms: coverage
reached screen 18 at 61,226,275 stage frames; legacy reached screen 16 at
43,239,627. Neither reached or defeated the boss. The unequal actual work and
different fresh prefixes prevent interpreting this as a matched-work Heat gain.
Both final trajectory witnesses replayed. No chain was restarted or extended.

A02 completed its 100,000 jobs: extremes reached screen 8 at 13,072,670 frames.
At the exact 100k boundary, coverage's identical-start trace had reached screen
16 at 12,528,426 frames. Coverage first logged screen 16 by 11,794,043 frames.
Extremes reached screen 8 earlier than coverage (6.36M versus 8.88M frames),
so the difference is later escape from that bottleneck, not uniformly faster
progress. Coverage retained 7,931 active entries/195MB logical memory versus
4,498/107MB; both used the same 8 GiB limit, with no pressure claim implied.

### A03: repeat the same-start result at a fixed frame budget

Freeze the A01 discovered Heat prefix and candidate build. Compare coverage
and extremes on new development seeds 20261102 and 20261103, 4 workers/8 GiB,
100k jobs and 12M admitted frames, 600s search plus 120s finishing per cell.
Use a 1,500s process-tree limit and 22 GiB hard RSS limit for the two sequential
paired panels. The original 20261101 diagnostic is re-scored from its existing
trace; no search is repeated for that seed. Score the greatest logged screen
whose admitted-frame counter is at most 12M. This is a conservative bound,
not an exact first-arrival frame, and excludes drained-window overshoot.

Seed 20261102: extremes on 0–3, coverage on 8–11. Swap placements for 20261103.
Keep one paired panel running at a time; all other msr1 experiments have ended.
This is repeated development from a fixed discovered start, not fresh chained
validation. Primary decision: only if coverage beats extremes in logged screen
attainment on at least two of the three seeds at this fixed budget does the
proxy qualify for more expensive depth experiments. Otherwise retain the
negative result and return to abstraction/exposure diagnosis. No default
promotion follows from this small diagnostic panel.

### K01: separate retention identity from selection geography

The P02 analysis qualifies a second implementation family: finer Metroid
retention identity, independently of resource coverage. The opt-in Cargo feature
`metroid-refined-archive` uses 8-pixel positions and raw pose at depth 0. All
coarser groups, resource preferences and per-slot capacity remain the legacy
ones. A distinct v9 policy identifier prevents cross-policy stream replay;
the default build keeps v8 field layout and values. This compile-time choice
keeps the historical key type unchanged while freezing two explicit executables.

First verify the exact partition refinement and all pixel/pose marginal mappings
without an emulator (10m watchdog). Then build immutable default and feature
variants (20m each), recording Cargo features in the build attestation. With
the Metroid ROM available, establish the ARM baseline with the frozen baseline
executable, run a 5k full-replay feature qualification, and require the new
default executable to match the ARM baseline at 100k jobs. Do not compare raw
stream hashes against ms02, whose core identity differs. Only then compare
fresh development seed 3 under default/refined identity at 500k jobs/70M frames,
4 workers/8 GiB, semantic/alphabet-only controls and 30m per cell. This is a
key-only ablation: neither optional resource retention policy is enabled.
More retained cells without better useful progress fails the gate; inspect
memory/exposure before any longer run. These are development seeds and cannot
qualify the breakthrough panel. Metroid ROM transfer was later approved.

A03 failed its preregistered escalation gate. At the fixed work ceilings,
the screen-attainment pairs (coverage, extremes) were (16,8), (8,8), (6,7)
for seeds 20261101–20261103. The two new extremes cells hit 100k jobs at
11.92M and 11.73M frames; coverage hit the 12M frame ceiling first. Thus
coverage does not win consistently even with slightly more actual frames in
those two cells. Do not run longer coverage chains or promote this policy.
The single-seed improvement remains a useful counterexample to universal
coordinate-extreme superiority, not a qualified search improvement.

K01 offline qualification passed: both default and feature builds pass all
119 NES library tests. The compiled default key merges all 16 numeric P02
pairs; the feature key separates 11, with every coarser group unchanged in
both outputs. Strict Clippy passes for the feature library and key-audit tool.
The coordinate/pose contract explicitly checks both independent coordinate
marginals so swapped-axis errors cannot hide in diagonal-only fixtures.

### Q03: freeze the prepared key-refinement executables

Build default and `metroid-refined-archive` variants from the same committed
source, sequentially against the owned target cache; 20m bound per build.
Then run B01's SMB and MM2 qualification cells through each executable, 5k
jobs, full replay, 240+60s per cell. All four streams must match B01 because
the new feature changes only Metroid. This completes the available cross-game
compatibility checks before asking for the still-required Metroid asset.
It cannot substitute for a Metroid replay or fresh-search result.

Q03 passed all four cells. Both frozen executables reproduce B01's exact SMB
and MM2 stream hashes with full campaign replay. Results and build attestations
are in [q03-results.json](q03-results.json). No Metroid emulator qualification
had run at this checkpoint because the ROM transfer awaited approval. The bounded next
experiments are prepared in `k01-smoke.json`, `k01-compatibility.json`, and
`k01-development.json`; [CHECKPOINT.md](CHECKPOINT.md) summarizes the evidence
and resumption order.

### T03/B02: observation validity before the Metroid key experiment

After the approved ROM transfer, the parallel research effort reported a
verified transient BCD health underflow. Its terminal correction at `e59a953a`
adds an opt-in `death_or_bcd_underflow_or_ending_v3` predicate and replay-policy
identity, with fixed observation counters. Import only its target, campaign,
evaluation request and runner changes; do not import local-search tools or
change the other worktree. Preserve v2 defaults and raw health. Its independently
replayed local probes support correctness/extendability, not fresh depth.

B02 establishes the original ARM Metroid 5k full-replay and 100k witness streams
from `baseline-001`, sequentially on CPUs 0–3 under an 1100s group watchdog.
The integrated default build must reproduce both. Both key variants then need
5k full replay under corrected terminal semantics before the key-only pair.
Hold terminal v3 fixed in both development arms; do not attribute its effects
to key refinement. The prior prepared feature executables remain archived,
but the corrected pair will receive new source and binary identities.

The initial test invocation used the parent Cargo workspace and failed before
running tests because NES is an independent package. Corrected the invocation
to its manifest; this was tooling setup, not a scientific outcome.

Integrated qualification passed locally: 122 NES library tests in each key
mode, strict Clippy for the feature library and evaluator, 22 runner tests,
and dependency boundaries. The exact imported prior diagnostics are retained
in `t03-prior-evidence.json`, with the source artifact's hash; they were not
rerun on msr1 or used as fresh search inputs.

### K01 development decision rule (before running the pair)

Use seed 3, corrected terminal v3 in both arms, unchanged semantic selector,
alphabet-only suffix generation, one representative per slot, 4 workers/8 GiB,
500k jobs and 70M admitted frames, 1800s search plus 120s finishing. The process
group has a 1980s hard limit. Default runs on 0–3 and refined on 8–11.
Record actual frames and replay every named milestone independently. For a
common-cost view use the last logged row at or below 10M, 25M, 50M, and the
smaller final frame total (capped at 70M); report that row's actual counter.
Final drained-window observations cannot be credited to a smaller boundary.

An additional replayed boss or ending is primary evidence. Otherwise, an
additional named capability or area beyond the initial Brinstar/Morph Ball,
or at least 20% fewer frames to a common later milestone, qualifies two more
bounded development seeds (4 and 5, swapping CPU placements for seed 4).
This is a practical escalation rule, not a significance test. If refined only
increases map/cell count, do not extend its horizon. Compare retained memory,
underflow eligibility, and unselected-removal fraction to distinguish loss of
representation from dilution of exploration. Mixed semantic results permit
the same small replication, never a default promotion. Longer depth runs need
a useful-progress win on at least two of the three development seeds; untouched
validation is still required for a breakthrough claim.

B02 passed both fixed-work cells: 691,673 frames at 5k jobs with full replay,
and 13,629,183 frames at 100k jobs with repeated witness verification. The
new corrected builds were compiled with Rust 1.97.1; the original baseline
used 1.97.0. Both comparison arms use the same new compiler and source;
legacy stream compatibility remains an explicit gate. The refined corrected
5k qualification exercised two real underflow endpoints, neither eligible
for admission, and passed full report/checkpoint replay. At this small budget
refinement retains 4,837 states versus 2,047, with the same ten observed maps;
this establishes activity and cost, not improved discovery.

### P03 preparation: separate new suffix evidence from the original examples

The existing equal-suffix probe had a fixed random seed and implicit legacy
terminal semantics. Add optional explicit terminal/seed arguments and record
both plus the suffix hash in a v2 report. Preserve the original defaults and
respect the chosen terminal predicate when counting living exits. This tool
change does not affect the frozen K01 binaries or either running campaign.

After K01 finishes, a bounded diagnostic may use the default arm's 16 sampled
equal-preference pairs, terminal v3, 64 suffixes of 24 actions, seed 20261201.
Those pairs have equal measured resources and capability identity, isolating
the remaining state aliasing. Reconstruct both endpoints exactly; classify
which pairs the compiled refined key separates and which still share a slot.
Count distinguishing living exits and survival separately, with actual prefix
and suffix frames. Use a 600s process-group bound on the idle little cores.
No diagnostic prefix enters fresh search, and no finite matching sample proves
equivalence. This tests new states/suffixes after choosing the representation;
it is not untouched validation of global search performance.

The live K01 throughput forecast places the slower CPU group's full work budget
near its 30-minute wall limit. Do not extend the limit. If a cell is censored,
the original full-budget gate fails; retain its completed, replayed evidence.
The already registered 50M checkpoint can support a separately labelled
diagnostic comparison only if both arms reach it. A follow-up prompted by that
checkpoint must use 50M from the start, rather than relabel the censored run as
a completed 70M panel. This operational contingency was recorded before
inspecting either arm's 50M checkpoint.

Follow-up [#281](https://github.com/pH14/harmony/issues/281) records the missing
distinction between boss-area entry, actual encounter, and partial fight
progress. Its source-labelled observations require their own qualification
and must not alter search policy merely because no boss was defeated.

K01 completed without wall censoring: both arms finished 500k jobs. Default
used 60,731,080 frames and refined 59,348,941; the shared comparison ends at
59,348,941. Both replay Morph Ball, missiles, Norfair, and one energy tank;
neither defeats a boss. Refined obtains the tank at 49.59–49.60M frames versus
59.16–59.17M, about 16% earlier. Missiles and Norfair are slightly later under
refinement. No additional final named milestone or 20% arrival improvement
passes the registered gate. Do not extend this key family to longer campaigns.
It retains 161,829 states versus 42,543, with 87 versus 84 cached map cells.
Results and conservative arrival intervals are in `k01-analysis.json`; full
paired provenance is in `k01-development-results.json`.

The P03 tooling qualification passed: two exact endpoint pairs, four suffixes
of eight actions, 8,342 prefix frames and 7,368 probe frames. The final P03
sample uses sixteen equal-preference pairs and a different registered seed.
Three sampled candidates are explicit continuations of their incumbents.

Two production-coordinator fixtures clarify exposure interpretation. A state
can be extended inside its birth job while retaining zero parent-selection
counts. Also, a previously executed pending job may receive its parent credit
only after an earlier admission removed that parent. Both fixtures pass;
comments and `theory.md` now state that removal counters capture admitted
parent accounting at removal time. They cannot alone establish lost first
exploration opportunities. No scheduler change is justified solely by K01's
larger unselected-removal fraction.

P03 completed within its bound using 1,885,036 physical frames including
prefixes. Across 1,024 paired suffix trials there were 66 discarded-only and
86 survivor-only living exits, with 48 cases where only the discarded state
survived. No new capability or boss was found. Fourteen of sixteen equal-resource
pairs have a useful disagreement; the compiled refinement separates six and
still merges eight. This is new evidence against interchangeability, not an
endorsement of either endpoint or of finer keys. `p03-analysis.json` retains
pair-level results and source hashes. Follow-up #283 records the exposure
accounting distinction established by the two coordinator fixtures.

### R03: ordinary representative plus job-ranked sample

Use `representative_job_sample_2_v1`: retain the existing best representative
plus the best candidate from the lowest-ranked creation job. The fixed rank
uses existing metadata and no search RNG; at most two entries share the same
byte budget. It works at equal resources and needs no finer Metroid key or
resource axes. The exact fixed-stream extrema invariant, idealized sampling
calculation and limits are in `theory.md`. Test loss cases as well as successes.

First qualify fixed-stream prefixes, same-cohort replacement, no-resource keys,
and full campaign/checkpoint replay with real alternative admission, pressure,
and continuation dispatch. Run generic and NES library checks and strict Clippy
under ten-minute bounds. Freeze one default-key ARM binary (20m build bound).
Require corrected Metroid legacy-policy 5k compatibility against K01, then 5k
full replay for the sample policy on Metroid and MM2. Actual alternative
admissions must occur. Qualify resource-extremes on the same executable as the
matched-capacity control.

Only after these checks, compare job sampling and resource extremes on fresh
Metroid development seed 3, terminal v3, legacy v8 key, unchanged semantic
selector/alphabet-only suffixes, 4 workers/8 GiB, 500k jobs/50M frames and
1800+120s per cell. The two arms use the same binary and disjoint big CPU sets;
sample uses 0–3 and extremes 8–11. Score last logged observations at or below
50M, with conservative milestone-arrival intervals and repeated witness replay.
The existing K01 default is an additional descriptive baseline, not a rerun.
An additional final named capability/area or a conservative 20% later-milestone
arrival improvement qualifies seeds 4 and 5; longer runs need wins on two of
three development seeds. Map/cell count alone does not qualify escalation.
A full search victory would be primary evidence but still needs fresh validation.
Do not interpret a censored full-work cell as a mechanism failure or success.

The transfer diagnostic is fresh MM2 Metal on seed 20261101, sampling versus
extremes under the same binary, 100k jobs/12M frames, 4 workers/8 GiB, existing
energy-splice vocabulary and 600+120s per cell. Run it after the Metroid pair
so no more than two four-worker campaigns overlap. A Metal result alone cannot
qualify Wily-depth recovery. Further chain work requires a useful matched-work
result and preserves the already recorded failed coverage/extremes ablations.

R03 local qualification: all 132 generic and 122 NES library tests passed;
strict generic all-target and NES library/runner Clippy passed. The updated
optional-policy fixture exercises real admissions, eviction, continuation,
and complete stream/checkpoint replay. The first focused compile missed a
test-only helper import; fixed before these checks. A duplicated full check
was needed because the previous tool response lost its completion status;
the logged repeat completed in under a minute. `analyze_p03.py` reproduces
`p03-analysis.json` byte for byte from the private raw output.

R03 ARM source is commit `33795855`, default features, source digest
`0429fe61af18ef06429e6ea2920fd8a69dc225fd25289eb31df37bcc55e22f97`;
binary `11249fff2bce1401d46d21db1f081678caae28c63124793886e78b5eb3992a7e`.
The frozen build finished in 44.62s with Rust 1.97.1.
Before development outputs exist, `analyze_r03.py` records the comparison
logic. For MM2, a useful transfer gate specifically means a replayed Metal
victory when the control does not win within budget, or at least 20% fewer
frames to their common victory. Screen counts alone do not qualify new chains.
Wall censoring blocks either game's escalation gate; report the completed work.

R03 qualification checker correction: the candidate passed full Metroid
replay with 2,352 alternative admissions. The early resource-extremes control
made 15,977 resource decisions but admitted no tradeoffs, producing the same
search decisions as legacy. The driver incorrectly required alternatives from
that control too and stopped after the Metroid cells. Require actual sample
admissions and actual control resource decisions; keep both full-replay checks.
The corrected external driver reuses the three completed cells and runs only
the absent MM2 cells. No frozen executable or source directory is edited.

R03 remaining portable checks passed: generic interface test, NES evaluator
binary test, all 22 Python runner contract tests, and dependency-boundary check.
The exact paired-tail examples in `validation-protocol.md` agree with exhaustive
enumeration for every success threshold and discordant sample size up to ten.

### E01: read-only boss-memory trace qualification

The current area labels do not establish that a boss was loaded or fought.
Before adding campaign counters, qualify a standalone diagnostic from existing
searched routes. It reads loader presence, six enemy slots (status, data index,
special byte, HP and position), and persistent defeat bytes. No value enters
archive identity, preference, selection, termination or fresh search.
Pinned disassembly `4270d57f` documents the loader and HP stores; its combat
code also overwrites the special byte. Therefore record raw bytes and sampled
loader/slot agreement, not an assumed permanent bit or a damage counter.

Use the already searched D01 Kraid-area tape only as a labelled diagnostic
input, never as a fresh-search origin. At most 8,192 actions/250k route frames.
Replay at ordinary chord boundaries, then twice with one-frame holds and
read-only memory inspection; require equal final emulator bytes and mechanical
state across all three runs and identical one-frame trace hashes. Stop on
terminal before applying any remaining tape. Build on little cores under20m;
run under300s, 4GiB and32MiB output. Record actual frames including setup.
The route may contain no boss encounter; that qualifies a negative control
only. Do not infer whole-campaign encounter absence or start longer searches
from an empty trace. A positive episode is required before proposing campaign
encounter or partial-damage counters.

R03 Metroid completed the full50M-frame comparison without censoring.
Sampling has no additional final milestone or20% arrival improvement, and
misses the control energy tank. Its Metroid escalation gate failed. Do not
run longer Metroid sample campaigns. Full results are preserved on msr1.
Both MM2 starts failed immediately because the driver incorrectly passed
the Metroid-only replacement-pair audit option. No emulator search ran.
Remove that unsupported diagnostic option, preserve failed outputs unchanged,
and run the same registered transfer conditions under new run ID `r03b`.
The binary, policies, seeds, budgets, vocabulary and selection stay frozen.

R03b MM2 completed with both victories replayed. Sampling used1,444,334 frames/
11,862 jobs; extremes used7,349,785 frames/57,310 jobs. The approximately80%
frame reduction passes the registered transfer gate. Both arms use the full-hold
suffix profile inherited from B01, `one_to_six_within_3_longest_actions_full_hold`.
This differs from C01's `one_to_six`; do not compare their resource-control costs
as if policy were the only difference. The R03b within-pair comparison is isolated.

E01 completed three replays of the searched92,904-frame Kraid-area tape, using
281,499 physical frames including setup. Ordinary and one-frame cadence ended
in identical emulator bytes/state; both one-frame hashes match. No loader or
active miniboss-tag agreement was observed. This is a qualified negative route
control only. No campaign encounter/damage counter or extra search is justified
by its empty trace. Raw source/input/build provenance remains private on msr1;
compact numeric results will be preserved here.

### J01: new-seed fresh MM2 chain after the R03b transfer gate

Question: does the strong Metal improvement generalize to a second development
seed and produce useful fresh chained depth? Use new development seed20261102
in two fresh chains, sampling versus extremes. No earlier gameplay input is
accepted. Every later stage uses only its own chain's searched victory and
replays the carried bridge twice. Keep R03b's full-hold suffix profile and
energy_splice:6 vocabulary, selector, four workers/8GiB,4096 actions and2/2
window/result slots; freeze the same `job-sample-001` executable. A driver
argument records the suffix explicitly while preserving its historical default.

Fixed stage order: Metal, Heat, Air, Wood, Bubble, Quick, Flash, Crash, Wily1–3,
then verify Wily4 entry with all eight weapons. Per stage:1M jobs/120M admitted
frames,1200s search+120s finish; each complete chain stops after5400s, outer
watchdog5460s. No retries of a failed stage or imported rescue prefix. Stop a
chain at its first unsolved stage. Run sample on8–11 and extremes on0–3, swapping
the prior placement; at most two campaigns. Wall throughput remains descriptive.

The first stage is a replication, later prefixes differ by their own search
histories, so later-stage comparisons are end-to-end chain evidence. Record
all setup/export/bridge/verification work separately. An earlier Metal win
without greater chained attainment or substantially cheaper common attainment
does not justify a longer chain. If both stop at the same stage, inspect their
completed common-work evidence before allocating another run. A Wily4 result
still requires the untouched repetition panel; this is development only.

Before J01, qualify the existing post-victory export for both completed R03b
victories without rerunning search, or use the already qualified chain export
path with a small full-replay chain-mode smoke if an independent export cannot
reuse those results. Bound extra qualification at5m per arm, preserve every
failed check, and keep all previously failed Metroid/coverage/refinement gates.

J01 export helper reuses existing adapter methods; the first compile named
the wrong provider trait for target creation. Corrected `TargetExecution`
import and strict Clippy pass. It will export both R03b victories and use the
frozen progress helper to replay both Heat bridges twice, with physical work
recorded. J01 itself still starts independently on development seed20261102.

J01 first export qualification stopped before a bridge: the extra helper
assertion required both an awarded boss and `dead == false`. Both R03b verified
witnesses actually report victory and death simultaneously. Their award bit
exists, but this does not yet qualify continued gameplay. The existing chain
export has no living-endpoint assertion; it requires ordinary award/menu
transition and then a separately replayed next-stage setup. Align the helper
with that contract, retain the raw endpoint and death flag, and require two
Heat replays with the Metal weapon retained before J01 may start. Do not change
any search, death, or victory predicate. If the bridge fails, do not claim
chainability or launch J01. Preserve the failed output and use new helper build
`metal-export-002` and a new qualification directory for this changed check.

J01 bridge qualification r2 passed for both R03b victories: twice-replayed
Heat entry has health28, weapon mask64, and lives4(sample)/3(extremes).
Additional export/setup/bridge work totals143,580 physical frames. Earlier
C01 victories have the same simultaneous death/victory flags and already
qualified their bridges; the extra living-endpoint assertion was inappropriate
for this existing export contract. J01 launched on the fixed new seed20261102
with its required binary and bridge gates, separate CPU sets, and watchdog.
