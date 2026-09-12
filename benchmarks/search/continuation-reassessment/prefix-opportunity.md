# Retained-prefix reuse does not earn the next experiment

CC01 failed independent confirmation. Before adding a new attempt-history
report or scheduler, inspect what the existing engine already avoids and what
the completed ordinary controls can say about remaining redundant work.
This assessment changes no policy, prior gate or ledger and runs no emulator.

The [audit](prefix-opportunity-audit.json), recomputed by
[this script](audit_prefix_opportunity.py), checks the exact native archives and
report hashes for all twelve completed CD01/CC01 cells. Relevant functions match
the registered source commit. All twelve final coordinator profiles account
for every admitted job and record **zero origin-replay jobs, actions and frames**.
An optimization of origin reconstruction has no measured work to remove here.
This statement is about these admitted jobs, not setup or out-of-job work.

The ordinary dispatch path already rejects a suffix when every executable
action-prefix input is currently retained. The six ordinary controls contain
8,037,314 executed jobs and 1,401 such skips: 0.01743% of their 8,038,715 draws.
Each control has fewer than 1,024 skips in total, so none can have reached the
1,024-consecutive-skip forced-execution guard. There is no missing ordinary
full-prefix duplicate filter to implement. Continuation-queue operations are
excluded from the quantitative argument below.

## A conditional bound for partially known prefixes

Consider a more ambitious shortcut that restores the end of a **contiguous,
currently retained prefix** of a drawn ordinary suffix and executes only its
unknown tail. Bound its opportunity on the original history, even granting
free lookup, restore and compatible observations. This does not bound a cache
of failed actions, retired/nonretained intermediates, queue policies or the
downstream utility of a changed adaptive history.

The mathematical premise is explicit: conditional on the history and selected
parent, the suffix length N is uniform on 1,...,6 and independent of a fresh
latent six-action tape. The source draws length first, then ordinary actions
without consulting length. Its deterministic Romu generator does **not** prove
this independent-random-draw premise. The result below is a model-based
opportunity bound, not a certified confidence statement about the native runs.

Hold the history and latent action tape fixed. Let L in 0,...,6 be the longest
initial sequence whose every action boundary has a retained input owner. Let
Y indicate an existing full-prefix skip. Let W be the largest amount of prefix
work the hypothetical shortcut could remove from this drawn job. Actions last
at most 120 frames. Therefore:

- If N <= L, the existing code skips the job: Y=1 and W=0.
- If N > L, Y=0 and 0 <= W <= 120L.

The remaining total-action cap can only turn additional draws into skips or
reduce W, so ignoring it is conservative. There are no forced executions in
the observed controls, as checked above. Conditional on the tape,

\[
E[\exp(W/1200-Y)]
\le {L\over6}e^{-1}+{6-L\over6}e^{L/10}\le1.
\]

The last inequality is checked for every possible L with exact rational
arithmetic. Use e^(-1) <= 3/8 and e^(L/10) <= 10/(10-L); their sufficient upper
bounds for L=0,...,6 are respectively
1, 427/432, 23/24, 101/112, 29/36, 31/48 and 3/8.
The first bound follows from the first four terms of the exponential series;
the second follows by bounding that series by the geometric series.

Averaging over the tape preserves the inequality for every adaptive history.
Consequently M_t=exp(sum(W)/1200-sum(Y)) is a nonnegative supermartingale under
the stated model, starting at one. Stop each process immediately before its
first guard-enforced duplicate execution; every observed control finishes before
that guard. Ville's inequality gives a simultaneous bound over all times of
this stopped process: except on a model event of probability at most delta,
sum(W) < 1200(sum(Y)+log(1/delta)). This is the standard nonnegative
supermartingale argument, not a new concentration theorem.
[Howard et al., Lemma 1 and section 6.1](https://arxiv.org/pdf/1808.03204v8).

Use delta=0.05/8 for each of the eight preregistered ordinary control
trajectories in CD01 and CC01, including the two unrun trajectories. A union
bound then covers the family without requiring independence between cells;
the per-cell bounds allow endpoint/futility stopping. Summing the six observed
controls' bounds gives **1,717,741.26 frames**, or **0.17017%** of their
1,009,463,959 admitted frames. This is not outcome pooling: neither development
nor confirmation's efficacy score changes.

The script also checks a counterexample to dropping the independence premise:
let the action tape depend on N, using N-1 retained actions and then a new
action. Every draw executes known prefix work when N>1, but none is skipped.
Thus low skip counts alone give no deterministic partial-prefix bound. The
native artifacts do not retain the complete job streams needed to measure
the exact partially known prefixes after the fact.

## Decision and limits

Do not fund a retained-prefix shortcut, another duplicate filter or new
attempt-exposure instrumentation from this evidence. Origin reconstruction is
zero in these cells; full-prefix filtering already exists; and even a generous
partial-prefix model gives little direct work to remove. There is no evidence
that a small shortcut would produce a compensating adaptive discovery gain.
This is an allocation judgment, not a theorem limiting eventual boss success.

The separate issue #283 remains valid: parent selections do not measure all
physical outgoing exposure. These aggregates cannot establish exact repeated
failed actions, useful unseen futures or a first-exposure scheduling benefit.
Do not replace that missing premise with a lifecycle fraction. The source
counterexamples and this opportunity bound avoid an otherwise unnecessary
instrumentation-and-pilot cycle; they do not nominate a new successful policy.

Recompute without emulation:

```sh
python3 benchmarks/search/continuation-reassessment/audit_prefix_opportunity.py --out /tmp/prefix-opportunity-audit.json
```
