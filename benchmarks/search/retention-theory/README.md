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
experiment. All comparisons below use the same frozen binary for both arms.
Its optional retention policy is disabled in controls.

msr1 has 12 ARM cores (four A520 and eight A720), 54 GiB RAM and about 792 GiB
free disk. At most two four-worker campaigns, each 8 GiB logical archive, may
run concurrently, with disjoint CPU sets 0–3 and 8–11. Four little cores remain
for compilation/verification; compilation may use idle big cores. Core classes
have different clocks, so matched-work results are primary. Paired placements
must be swapped before interpreting throughput. Bound total output to 80 GiB,
each cell to 4 GiB, and each experiment with the runner's process-group watchdog.

The licensed Metroid asset transfer from ms02 awaits explicit approval after
automatic approval review rejected it. MM2 and SMB assets already on msr1 have
the expected hashes. Source, theory, and those workloads can proceed meanwhile.

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
before execution and has not been retried. Neither consumed search work.

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
