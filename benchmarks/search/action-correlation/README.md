# Changing one control component at a time

The completed depth-transfer gate failed, and the qualified E03 diagnostic found
no classified encounter in eight selected development tapes. Those findings do
not establish campaign-wide absence or explain the causal failure. This study
examines a distinct, inexpensive action proposal; it claims no game improvement
yet. Retention, parent selection, observations, terminal semantics and fresh
origins remain fixed. Existing failed duration and continuation experiments
remain failed.

## Literature and choice

[Planning Goals for Exploration](https://arxiv.org/abs/2303.13002) chooses goals
for their subsequent exploration potential using learned world models and a
goal-conditioned policy. [Go-Exploit](https://arxiv.org/abs/2302.12359) combines
archived restart states with AlphaZero policy/value learning. They motivate
measuring useful continuation rather than static novelty, but their learning
systems are absent here. Neither paper justifies a new frontier bonus or a claim
that our archived states are interchangeable. Adding their full machinery is
not the cheapest next falsifier.

[TAAC](https://arxiv.org/html/2104.06521v3) conditions an act-or-repeat decision on
a sampled action and learns the decision with a critic. It supports examining
temporal coordination, not a claim that unconditional persistence improves NES
search. The proposal below is our fixed, finite kernel, not TAAC and not a new
literature theorem. It needs no learned model, target coordinates or game route.

The parallel duration variants call the same independent direction/A-B sampler
and alter whole-chord hold lengths. Here each suffix starts with the existing
independent sampler, including its exact action count, durations and special-tap
positions. An additional deterministic random stream changes only correlations
between consecutive ordinary button combinations. No state or controller
history is inherited from an archived origin.

## Frozen proposal and a necessary control

An ordinary command is a tuple in `D × A × B`, of size `9 × 2 × 2 = 36`.
The direction component uses the existing nine legal sets, including diagonals;
the two button components use the existing two bits. Let `μ` be uniform on
these commands and `R_i` replace only component `i` with a fresh draw from its
existing alphabet. The candidate kernel is

`K(x,y) = 1/2 μ(y) + 1/6 [R_D(x,y) + R_A(x,y) + R_B(x,y)]`.

The half-mixture is fixed before game outcomes; it is not fitted or swept.
The first ordinary command of each suffix, and the first after a special tap,
remain the independent proposal. Metroid Select and MM2 Start taps keep their
original law and clear correlation. The transform sees no ROM observations.

This changes the chance of repeating the complete command from `1/36` to
`43/216`. Therefore comparison with independent commands alone cannot attribute
a benefit specifically to changing components independently. A second control
repeats the entire previous command with probability `37/210`, otherwise draws
from `μ`. It has the same `43/216` self-transition probability and exactly the
same truncated geometric full-command run-length law as the candidate. Special
taps and durations also have the same law. A candidate gain over that control
would distinguish component correlation from whole-command persistence.

## Exact checks and limits

[derive.py](derive.py) enumerates the 36-command kernels using rational numbers.
[Its result](exact-model.json) checks row/column sums, reversibility, full support,
the stationary law with special taps, and the matched complete-command run
lengths. Uniform one-command marginals are preserved from an independent first
command. Mean proposed duration is `505/12` frames per chord, unchanged; for the
existing uniform one-to-six shape the mean proposed suffix is `3535/24` frames.
A production transform must additionally preserve each base suffix's durations,
length and special taps **path by path**, not just in expectation.

Constructed four-command finite-state tasks give both a favorable example and
a counterexample. These condition on ordinary commands and are not NES success
metrics:

| Proposal | Keep direction while changing A at least once | Change all three components at every boundary |
| --- | --- | --- |
| Independent | 0.0012003 | 0.0109739 |
| Component half-mixture | 0.0416095 | 0.0013717 |
| Matched whole-command repeat | 0.0082548 | 0.0061354 |

The first pattern is more likely under the candidate even after matching whole
command runs. The second is less likely: there is no reward-independent utility
or discovery-rate dominance. An injected whole-command replacement passes the
run-length check but fails the component-correlation claim. These cheap checks
establish a distinct mechanism and expose its downside; they cannot establish
that either pattern predicts a game milestone.

The mathematical laws assume ideal independent uniform draws. The repository's
finite PRNG and multiply-shift bounded draws do not prove that idealization;
implementation and replay checks are separate. In particular, deterministic
seed separation is not a proof of statistical independence. Actual emulator
work can also differ after death/terminal boundaries despite identical proposed
frame totals. Performance panels must compare actual work and resources.

## Decision gate

The exact finite checks pass without emulation. This earns only an explicit,
default-preserving implementation and bounded native qualification. Require
exhaustive finite transition checks, duration/tap preservation, suffix-local
reset behavior, deterministic replay, policy-identity rejection, and unchanged
legacy bytes before any fresh performance panel. Keep the same operator for
Metroid and MM2; do not fit separate game parameters. No long experiment follows
from the favorable constructed pattern alone.

A subsequent fresh paired development panel must freeze one useful calibrated
endpoint, seeds, horizon, resource bounds and futility rule before dispatch.
A whole-command matched control is required before attributing any gain to
component correlation; an ordinary-control comparison and independent
confirmation remain necessary for useful-search claims. Boss/Wily validation
retains the original untouched-seed criteria. The full goal is still active.
