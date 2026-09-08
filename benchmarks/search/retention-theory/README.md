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
