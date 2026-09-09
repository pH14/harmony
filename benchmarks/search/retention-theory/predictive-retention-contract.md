# A decision contract for continuation-based retention

This is a design obligation for the next experiment, not a new running policy.
It uses the completed P03 data to distinguish detecting an abstraction error
from choosing a beneficial replacement. R05's frozen milestone gate is unchanged.

## Measure the whole replacement

Fix an action-suffix distribution D, horizon, allowed events and nonnegative
event weights whose sum is at most one. Let C(s,z) be the events exposed by
replaying suffix z from state s, under the same action/frame allowances and
terminal semantics. For a retained set R, define

```
U(R,z) = sum_e w_e * 1[e belongs to union(C(s,z) for s in R)]
```

Suppose a proposal changes R into R'. Its exact finite-suffix change is
`Delta(z) = U(R',z) - U(R,z)`. Equivalently it is the weight of events gained
outside the old union minus events lost outside the new union. For adding a
candidate c without removal, the gain is the weight of
`C(c,z) minus union(C(s,z), s in R)`. Comparing c with just the worst survivor
does not compute this marginal when a second survivor covers the same events.
The production-archive counterexample in the theory fixtures demonstrates that
specific failure. Replacement needs both the gained and the lost sets.

Keep action conditions in this object: the feature is `(z,event)`. Pooling events
across different suffixes can treat a state that succeeds on left as equivalent
to one that succeeds on right. Conversely, exact preservation for every action
is stronger than preserving expected utility under one fixed D. State the claim
being tested; neither a motion tuple nor a fitted average supplies the stronger
claim automatically.

## The existing data reverses direction across objectives

[The reproducible objective audit](p03-objective-audit.json) checks every raw
P03 exit flag against its underlying map sets and the original provenance.
Across the selected 16 pairs and shared 64 suffixes, discarded states expose
124 exclusive map-and-suffix events and survivors expose 112. Unit weighting
of these pair-local features therefore favors discarded states by 12 events.
Endpoint survival gives 326 versus 353, favoring survivors by 27 trials.
Neither calculation is a boss-progress result or a population estimate.

The original 66 versus 86 exclusive-exit trial counts answer another question:
whether at least one exclusive map exists on a suffix. Sixteen trials have
exclusive maps on both sides. Those flags are not mutually exclusive successes,
and subtracting them does not measure the number of futures gained or lost.
Living map visits inside an action also do not imply a living final endpoint.

Thus P03 demonstrates aliasing but does not identify a globally preferred
replacement rule. Objective choice can reverse the answer on the very same
replayed data. Preserve named task events and survival separately until a
weighting is justified; do not choose weights after seeing which policy wins.
The analysis performs no new emulator work and does not retune any campaign.

## Put uncertainty and probe cost into the design

For a fixed comparison with independently drawn suffixes, normalized utility
gives Delta in [-1,1]. Applying [Hoeffding's bounded-variable inequality](https://www.tandfonline.com/doi/abs/10.1080/01621459.1963.10500830)
to this range and both tails yields

```
P(abs(mean(Delta) - E[Delta]) >= epsilon) <= 2 * exp(-n*epsilon^2/2).
```

For M fixed comparisons, a union bound makes the right side
`2*M*exp(-n*epsilon^2/2)`. Suffixes may be shared across comparisons: the union
bound needs no between-comparison independence, but each comparison still needs
the stated independent draws. At M=16, alpha=0.05 and epsilon=0.1, this
sufficient bound requires 1,293 suffixes per comparison. At n=64 its simultaneous
half-width is about 0.449. These loose worst-case bounds explain why a handful
of matched probes can refute equivalence much more easily than certify useful
replacement. They are model calculations, not confidence statements about the
deterministic, adaptively selected P03 sample. Raw event-count utility must be
normalized before using these bounds.

For an adaptive comparison list or stopping rule, freeze a new confirmation
suite or derive an appropriate sequential bound. Reusing the same suffixes to
choose a descriptor and then certify it does not supply independent confirmation.
No statistical precision compensates for choosing the wrong task utility.

Charge reconstruction, every executed suffix, snapshot storage, export and
verification before allocating search work. Terminal branches may use fewer
frames under the same allowance; dying early is not an efficiency improvement.
Any candidate using probes must beat its control under a total physical-work
budget that includes those probes, as well as under the archive-memory budget.

The completed [U01 diagnostic](u01-analysis.json) captures the complete local-rule
proposal and applies this union comparison. [H02](h02-analysis-with-reference.json)
preserves the action and horizon conditions, showing losses within ordinary
one-to-six-action jobs. Every measured triple has a two-state subset covering
its offered positive-event union, including all short and long horizons jointly.
That is an exact hindsight result on the selected bank; it does not identify a
cheap online predictor or show that two representatives always suffice.

The next design must freeze its available features, predicted conditional
outcomes, replacement decision and total cost before an independent diagnostic
suite. Predicting a discarded state's usefulness alone is insufficient: measure
what the proposed survivors lose as well as gain. Existing trajectory fragments
may reduce data-collection cost, but their adaptive selection does not make them
an independent confirmation set. Preserve the original milestone gate before
allocating fresh-search compute. This addresses
[#286](https://github.com/pH14/harmony/issues/286); it authorizes no further long
campaign in this tranche. A callback before global eviction does not certify
the final globally active set; that boundary remains explicit in the result.
