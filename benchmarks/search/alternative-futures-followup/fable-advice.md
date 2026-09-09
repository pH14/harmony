# Alternative futures: revised goal and first preregistered step

## Context

The 12-hour tranche ended without a breakthrough. The untouched Metroid panel
has 0/10 boss successes in both terminal arms. Both fresh fixed-order MM2 chains
obtained eight weapons and stalled at Wily 1. The two headline diagnoses are not
yet validated as useful or class-level correct:

- The paired living-exit asymmetry (156 discarded-only vs 42 survivor-only exits)
  is measured relative to the survivor, not the archive, and its unit of
  inference is 16 correlated pairs.
- The terminal defect was mechanically verified on one inspected endpoint. The
  summary generalizes to all 87 control underflow endpoints and to every state
  the v3 predicate excludes in corrected arms, whose count is not reported.

Both arms scoring 0/10 is a floor effect. Nothing in the panel can tell whether
any campaign ever reached a boss room. Until that is known, boss defeat is not a
sensitive outcome for any mechanism.

This plan is advice only. It was produced without running commands or changing
repository files.

## Revised objective

Localize the fresh-search bottleneck in both games and settle the validity of
the two headline diagnoses using existing artifacts, before building any new
retention mechanism or spending a new untouched panel.

Completion: a decision memo recording the predeclared outcome of analyses A-E
below and the resulting go/stop decision for the conditional pilot.

Stop conditions (objective ends when any holds):
- All five analyses are recorded with their decision rules applied.
- Budget cap reached: under 5% of the last tranche's measured CPU and under
  25M new physical frames in total. No new campaigns in this objective.
- A wall bound is exceeded twice on the same analysis without a diagnosed cause.

Budget assumption: the user has not set a budget. The caps above are proposed
and should be confirmed or replaced before execution.

## Analyses (preregistered)

Order is by information per cost. E and the reanalysis half of B cost no
emulator work and should run first.

### E. Zero-compute calibrations (existing panel and chain artifacts)

1. Null dispersion: distribution of control-vs-control coverage and milestone
   work differences across the 10 T04 seeds. This sets the margin any future
   paired claim must exceed.
2. Corrected-arm exclusion counts: per seed, number of endpoints v3 excluded
   that v2 would have admitted. Compare against the 87 control underflow
   endpoints. Correlate with per-seed coverage and Bombs-work deltas
   (descriptive only, n=10).
3. MM2 chain comparison: entry-state resource profiles and frontier watermark
   for both chains. Same watermark from different entry states weakens the
   entry-state hypothesis.
4. Frontier exposure from existing counters: for living cells at each MM2
   chain's watermark, admitted count, replacement count, and admitted
   parent-job credit.

Decision: E1 defines the margin used in the pilot. E2 with exclusion counts far
above the underflow counts is prima facie over-inclusion and raises A's priority.

### A. Terminal settle audit

Question: does v3 exclude any state that is not dead regardless of input?

- Sample: all 87 control underflow endpoints plus every v3-excluded endpoint
  from the corrected arms (from E2), all 256 controller masks each.
- Control: 200 randomly sampled admitted, non-underflow endpoints of similar
  adapter health, same protocol. Verifies the harness does not mark living
  states dead.
- Observable: adapter health and reserve readings after a settle criterion
  (stable for m frames or zero). Classify dead only if health is zero and no
  reserve remains under every mask.
- Bound: replay only. Physical frames under 3M. Wall under the 10-minute
  focused-check limit per batch.
- Decisions:
  - Zero excluded states settle alive: v3 is correct for the class. Attribute
    the panel regression to run dispersion. Keep v3 versioned, no default
    change, close the terminal line. R-plus-T then depends only on B and the
    resource-arm count below.
  - Any excluded state settles alive: v3 is over-inclusive. Correct the summary
    and issue text. Define v4 requiring the settle test. The T04 comparison is
    not a test of correct semantics. Retest v4 only at development anchors
    against existing control references. Halt any R-plus-T campaign until v4.

Resource-arm count (cheap, same batch): number of retained resource extremes in
existing resource-arm artifacts that are underflow endpoints. Near zero means
the R-plus-T interaction cannot explain the resource failure; do not run an
R-plus-T campaign.

### B. Paired-probe reanalysis and destination novelty

Question: is the discarded-only living exit archive-novel and predictable from
cheap state features, or only survivor-novel and positional?

- B1 (no compute): per-pair counts of discarded-only and survivor-only exits.
  Sign test over 16 pairs. Record how the 16 pairs were sampled and the
  survivor rule in force.
- B2: resolve each discarded-only exit's destination cell key. Check archive
  membership at the pair's replacement work and the control campaign's first
  discovery work for that key. Bound: zero frames if endpoints carry keys, else
  replay of the 1,024 recorded suffixes (about 7.2M physical frames).
- B3: for each pair, record source within-cell descriptor (adapter position
  descriptor), health, trajectory length. Test whether any cheap feature
  predicts the exit asymmetry across pairs.
- Decisions:
  - B1 fewer than 12 of 16 pairs favor the discarded state: aggregate is a
    few-pair anecdote. Downgrade the evidence; do not build retention on it.
  - B2 zero destinations archive-novel at replacement work: local loss is
    globally redundant at this granularity. Stop the #286 line.
  - B2 archive-novel but discovered by control within 10% of anchor work after
    replacement: speed-only potential. Proceed only if the delay is material
    relative to the milestone work established in C.
  - B2 archive-novel and never discovered in at least 3 pairs: strongest case.
    Pilot is warranted, subject to C.
  - B3 a cheap feature predicts the asymmetry: pilot mechanism is
    descriptor-diversity retention, not probe-based retention.
  - B3 nothing cheap predicts it: estimate probe cost per replacement event
    times replacement events per campaign. If above 5% of the frame budget,
    probe-based retention is infeasible at equal physical work. Stop.

### C. Metroid stage localization (existing checkpoints)

Question: where along area entry, boss-room entry, encounter, damage, defeat do
the 20 campaigns and the 100M area probe stall?

- First check whether the archive key or coverage export already identifies
  boss rooms. If not, add reporting-only adapter decoding for boss room,
  encounter active, and damage dealt (#281). No searcher use of these fields.
- Re-export from final checkpoints. Zero or trivial emulator work.
- Decisions:
  - Boss room never entered in any campaign: navigation bottleneck. Boss defeat
    is unusable as a panel primary. Boss-room entry becomes the primary
    observable for any future panel. Retention pilot allowed if B supports it.
  - Entered, no encounter or damage: encounter bottleneck. Retention line ends.
    Local exploration from own-development boss-room starts becomes the
    candidate line, which requires a new goal decision from the user.
  - Damage dealt: combat bottleneck near success. Same as above, with damage
    fraction as the milestone.

### D. MM2 frontier extendability audit

Question: does the Wily 1 frontier stall from failure to extend, or from losing
extensions?

- Starts: from each chain's own Wily 1 archive, the living cells with the
  highest adapter stage-progress descriptor (frontier) and cells at the median
  descriptor (calibration). Own-development diagnostic starts, labeled as such.
- Protocol: N ordinary suffixes per cell using the existing paired-probe suffix
  length. Classify each outcome: death, same cell, previously known cell, new
  cell. Record frames of any automatic non-controllable segment.
- Bound: about 8 frontier plus 4 calibration cells per chain, 64 suffixes each,
  under 12M physical frames total, under the 20-minute development limit per
  chain.
- Decisions:
  - Frontier extension rate near zero while calibration is clearly positive:
    local exploration or dead-time bottleneck. Retention transfer unwarranted.
    A duration-distribution arm at the frontier is the next diagnostic, at
    equal frames.
  - Extension rate positive but E4 shows extended cells replaced or never
    selected: retention or selection implicated. MM2 becomes a transfer target
    for the pilot.
  - Frontiers differ across chains (from E3): entry state matters. The
    transfer experiment is chain-level alternative carry (below).

## Conditional pilot (only if B and C both permit)

Mechanism: generic second same-cell representative in the searcher,
`descriptor_diverse_2` (adapter-provided descriptor distance) if B3 found a
cheap predictor, else `probe_behavior_2` only if its probe cost fits the
5% physical-work bound. Equal 8 GiB logical memory. Selection, continuation,
vocabulary, workers, budgets fixed. Same terminal predicate in both arms.

- Seeds: 5 of the inspected T04 seeds. Controls: the existing T04 control
  anchors, valid only if the mechanism-off build reproduces the control stream
  hash on the qualification gate.
- Staging: run 2 seeds first at the existing 3M / 400M anchor, registered as
  long-horizon probes. Continue to 5 only if neither is worse than its control
  on the primary by more than the E1 margin.
- Primary: work to the milestone chosen by C, common reporting. Secondary:
  archive-novel cell count at common work; exposure of second representatives
  as dispatched jobs (dispatch-time counter, #283 minimal); lineage attribution
  of the milestone-first witness through a retained second representative.
- Decision: proceed to a new panel only if at least 4 of 5 beat the control
  reference on the primary by more than the E1 margin, and lineage passes
  through a second representative in at least half the improved seeds.
  Otherwise stop the retention line and record.

## Minimal implementation

- A: batch driver over endpoint hashes reusing the existing 256-mask raw
  follow-up, plus a settle criterion. Focused test on one known-dead and one
  known-living endpoint.
- B2: destination key export from the probe tool if absent; first-discovery
  work per cell key from campaign logs.
- C: reporting-only adapter fields for boss room, encounter, damage (#281),
  only if keys lack them.
- D: reuse the paired-probe suffix runner with own-archive starts.
- Pilot only: dispatch-time exposure counter (#283 minimal); second
  representative policy in the generic searcher; lineage attribution export.
  Replay-qualify under actual alternatives, eviction, and continuation
  dispatch, as done for `resource_extremes_2_v1`.

## Evidence required before any new untouched panel

1. A resolved. If v4 exists, it excludes the 87 and passes the settle audit
   with zero false exclusions.
2. C resolved and the panel primary observable chosen accordingly.
3. Pilot passed its decision rule on inspected seeds with lineage and exposure
   evidence.
4. Mechanism-off build reproduces T04 control stream hashes on the gate.
5. Equal-memory and equal-physical-work accounting includes probe frames.
6. Margin exceeds the E1 null dispersion.

## MM2 transfer without unchanged chain reruns

- No fresh chain rerun until a stage-local gate passes.
- Stage-local gate: from each chain's own Wily 1 entry state, matched 50M local
  work, mechanism arm vs control. Primary: stable frontier advance, defined as
  a new cell beyond the current watermark holding a living state with at least
  N admitted descendants at end of work. A transient visit does not count.
- If D says local exploration, the stage-local arm is a duration-distribution
  variant instead of retention, still diagnostic.
- If E3 says entry state matters: chain-level alternative carry. Carry two
  own-chain victory endpoints with distinct resource profiles into Wily 1 at
  equal total local work, reporting bridge cost. This is a chain-policy change
  and must be labeled as such.
- The 5 untouched MM2 seeds remain reserved for a final panel after a chain
  rerun with a mechanism that passed the stage-local gate.

## Facts, assumptions, missing evidence

Facts from the tranche: 87 control underflow endpoints, 0 corrected; 0/10 boss
in both arms; both chains eight weapons, Wily 1 stall; 156 vs 42 exits across
16 pairs; corrected coverage lower on 7 of 10 seeds; execution budget bound
the anchor, not frames.

Assumptions: survivor rule prefers score including health; probe endpoints can
be resolved to cell keys; archive keys may not identify boss rooms; adapter
health can transiently underflow with a reserve remaining.

Missing evidence that could change the recommendation: the v3 exclusion count
per corrected campaign; per-pair probe counts and pair sampling rule; whether
any campaign entered a boss room; whether both chains stall at the same
frontier; the cost per probe-based replacement decision.

## Verification

- A: control sample of living endpoints classified alive; one known-dead
  endpoint classified dead.
- B2: destination keys match archive keys for a hand-checked subset.
- C: re-export from a checkpoint reproduces existing coverage counts.
- D: calibration cells show positive extension rate.
- Pilot: mechanism-off stream hash equals T04 control; replay of any claimed
  milestone witness twice from ordinary genesis on independent targets; Nova,
  STB, SMB gates unchanged.

