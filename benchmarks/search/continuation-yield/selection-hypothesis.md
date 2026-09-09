# Selection cost is a prior, not an oracle

## Mechanism and prediction

The current uncounted semantic selector already downweights groups with repeated
failures to produce new-cell descendants. Adding a rolling yield average would
add an estimator and parameter without first testing a distinct causal idea.
Instead, remove its historical group-time ranks under one new explicit policy:
`room_cell_uniform_128_energy_progress_no_cost_v1:3,6,12,2`.

At a between-cell draw the old weight is
`(barren_energy << 16) >> min(16, novelty_rank + cost_rank)`;
the candidate replaces the sum with `novelty_rank`. Within the newest sampleable
window, up to 128 members, the old weight is `256 >> min(8, floor(rank/4))`
after sorting by `(time_in_group, id)`. The candidate uses 256 for every member
in that same order. The uniform quarter, semantic class walk, barren updates,
novelty, retirement, retention, action alphabet and suffix distribution stay fixed.

`time_in_group` measures time since the trajectory entered its coarsest group;
for Metroid, that is the coarse progress class. It is neither total route length
nor the snapshot restoration cost. All four D01 controls used resident snapshots
without eviction and dispatched zero replay jobs. This removes one possible
direct physical-cost rationale for the rank, but cheap states could still have
more remaining action budget or better futures. Their utility is an empirical
question. Removing the rank also removes an age preference among equal-cost
entries and changes novelty's effective gradient when its old combined rank hit
the cap. We will not attribute a combined result separately to these effects.

**Directional hypothesis:** removing this prior lowers mean restricted
first-missile cost at 25M frames in a fresh paired Metroid panel. The hypothesis
can fail. A fixed archive whose expensive members have the only useful exit
favors the new policy; the mirror, whose cheap members have that exit, favors
the old one. Executable tests use the actual selector, including its uniform
quarter, and demonstrate both directions. They also exercise between-cell ranks
past the actual scale of four, preserve novelty/energy under tied costs, check
irrelevance of numeric map labels and replay under memory pressure/concurrency.

## What can be derived before an emulator run

For a fixed archive and continuation distribution, let `p_i` and `q_i` be the
old/new selection probabilities, and `y_i` the expected useful outcome from
parent `i`. Then the one-step difference is exactly
`Delta = sum_i (q_i - p_i) y_i`. When `p_i > 0`, this also equals
`Cov_p(q_i/p_i, y_i)`. The sign requires information about useful futures; it
cannot be proved positive from shorter history or increased diversity alone.

For an outcome in [0,1], `abs(Delta) <= TV(p,q)`, where
`TV = sum_i abs(p_i-q_i)/2`. With integer weight vectors `a,b`, this is exactly
`sum_i abs(a_i*sum(b)-b_i*sum(a)) / (2*sum(a)*sum(b))`.
The optional diagnostic computes this at the two encountered conditional
selection stages. These are not full-parent probabilities or a global bound:
different choices change subsequent active states, energy and windows. A tiny
per-draw difference can accumulate; a large one says nothing about the sign.
The statistic needs no sampled empirical frequency estimate.

The second consultation's proposal to infer direction from uniform-draw yield
strata is not adopted as a gate: existing logs do not carry those conditional
outcomes, and observational cost/yield association would still need careful
conditioning. Its statement that low conditional TV proves the intervention
cannot matter is too strong for an adaptive campaign.

## Qualification before screening

Freeze source, build features and binary identity. Repeat D01's first registered
seed under the unchanged old selector at its exact 25M-frame bound, with the
optional diagnostic enabled. Require the same complete campaign-stream hash,
frame/execution totals and witness outcomes as the frozen baseline. This is
qualification work charged to the 50M auxiliary allowance, not a fresh control
or a performance pair. Inspect conditional influence on this entire fixed path;
zero changes everywhere means the ablation has no identified opportunity on
this path and requires reassessment before a panel. Nonzero changes merely
establish an operative intervention, not likely improvement.

Run bounded native candidate smoke cases with full campaign replay in Metroid
and MM2. Only then register four separate fresh paired seeds, the fixed endpoint
and unchanged 25M-frame/15-minute limits. At least three strict interval-robust
wins and 15% lower mean restricted cost are needed for an exploratory pass.
Independent confirmation and MM2 transfer remain necessary before escalation.

## A limited deduction from D02

The qualified control trace recorded 124,702 changed between-cell distributions
across 145,455 such draws, and 32,715 offered terms whose positive cost rank
reached the combined cap. Every draw affected by at least one such capped term
contributes at least one to that term count. Therefore at least
`124702 - 32715 = 91987` changed draws (63.24% of all between-cell draws, or
73.77% of the changed draws) had no such capped term. This establishes that the
intervention often changes the ordinary cost preference even away from the cap;
its operation on this trace cannot be explained solely by changing novelty's
behavior at the cap. Within-cell distributions were unchanged throughout D02.

This is a counting bound on the encountered baseline trace. It does not apportion
an adaptive performance gain between selection stages, establish useful futures,
or extend to every archive visited by a different candidate trajectory. The
registered S01 performance criterion is unchanged.

## What the completed S01 traces localize

[The read-only S01 mechanism summary](s01-mechanism-analysis.json), derived from
the final recorded diagnostics in [the frozen results](s01-results.json), extends
the baseline observation to both arms. Across all four controls and four
candidates, **zero within-cell distributions changed** on the recorded paths.
Between-cell distributions changed on 513,916/589,553 control draws and
529,780/601,172 candidate draws. Their draw-weighted mean conditional TV values
were approximately 0.28332 and 0.27623 respectively. These are different
encountered histories, not paired estimates of the same archive distribution.

There is also an exact size deduction. The old within-cell vector starts with
four weights of 256, followed by 128 at index four; the new vector is constant
256. Their normalized laws are equal if and only if the offered window has at
most four members. Thus all 1,190,725 audited within-cell draws in S01 offered
at most four members. This follows from the implemented rank formula and zero
changed-distribution counts, not from an average occupancy or a sampled maximum.
It applies to these recorded draws, not every reachable archive or future seed.

The cap-count deduction also applies separately: at least 392,327 control and
439,811 candidate draws changed without any positive-cost term at the combined
cap. The operative selection difference on these recorded paths is therefore
between cells, and it often acts away from that cap. The summary does not
separate the cost preference from its interaction with novelty along changing
histories, nor prove how either caused the observed precursor improvement.
No snapshot eviction was recorded in either arm. No new emulator work or
parameter choice was used for this analysis; independent confirmation proceeds
with the originally qualified mechanism.
