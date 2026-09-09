# What a seed panel can establish

The goal's minimum repeatability targets are operational thresholds: 3/10 fresh
Metroid boss seeds or 3/5 fresh MM2 Wily 4 chains, accompanied by matched-control
improvement and evaluation on both games. They do not alone certify statistical
significance. No development outcome changes this distinction.

## Freeze before validation

Select one mechanism, executable/source/asset identities, memory and admitted
work limits, terminal predicate, selector, suffix distribution, and stage order.
Generate a fixed list of previously unused seeds independently of their search
outcomes, after that freeze. Exclude historical/development seeds and the other
research effort's development series. Record the seed-generation procedure and
the complete list before launching any validation cell. Both arms use each seed;
swap CPU placements between pairs. Do not tune on validation outcomes or silently
replace unsuccessful seeds. A revised candidate needs a new panel.

Record every cell's outcome, admitted and physical work, verification, memory,
and censoring. Victory or partial-progress claims require replayed witnesses.
A watchdog protects resources; a wall-censored cell is not automatically a
completed matched-work failure. Show it separately and withhold the complete
panel claim if its prescribed work was not observed. Do not drop it from a
success-rate denominator and report the remainder as the original panel.

## Exact paired comparison

For a fixed game and work budget, each independently sampled seed supplies two
binary outcomes: candidate and control attain the declared endpoint or do not.
Let w count candidate-only successes and l count control-only successes. Under
equal success probabilities, the two discordant outcomes have equal probability.
Conditional on d = w + l discordant pairs, W therefore has a Binomial(d, 1/2)
distribution. The one-sided exact tail for a candidate advantage is

```
p = sum(comb(d, k) for k in range(w, d + 1)) / 2**d
```

When d = 0, p = 1. This calculation assumes independent seeds sampled from the
declared seed population, a frozen comparison, and fully observed outcomes.
Deterministic execution permits exact replay; it does not make the seed sample
representative or justify treating correlated suffix probes as independent seeds.

Three candidate-only successes and no control-only success give p = 1/8,
whether observed in five or ten total pairs. Five such discordances give
p = 1/32. These are exact calculations, not a post-hoc rule that a run should
continue until a favorable p appears. Report w, l, joint successes, joint failures,
and the raw operational threshold alongside the inferential calculation.

The two games and many development variants do not constitute one preregistered
single comparison. If making an inferential claim about success on either game,
account for the two primary comparisons (for example, a fixed 0.025 threshold
for each), or perform a new independently specified confirmation. A five-pair
MM2 screen cannot cross that example threshold even with five candidate-only
successes. It can still provide a useful operational result. Intermediate
milestones are diagnostic endpoints, not interchangeable primary endpoints.

## Failure to observe is limited evidence

For n independent Bernoulli attempts with success probability p, observing no
success has probability (1-p)^n. Inverting that probability at 0.05 gives a
one-sided upper confidence endpoint of 1 - 0.05**(1/n): about 0.259 for n = 10
and 0.451 for n = 5. Thus even a complete small all-failure panel does not prove
an approach incapable of success. A censored panel or adaptively chosen seeds
does not satisfy the premises of this simple calculation.

The 20% precursor gates and bounded development panels are compute-allocation
rules. They can reject an approach whose advantage appears only much later.
Their purpose is to prevent expensive escalation without positive evidence;
passing them is not a theorem about eventual boss or Wily attainment, and
failing them is not a universal impossibility result. Preserve both favorable
and adverse finite counterexamples, fixed-suffix diagnostics, and every failed
gate when explaining the final choice.
