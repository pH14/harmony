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

The [independent spectrum check](verify_spectrum.py) applies each frozen matrix
to a complete tensor-contrast basis. For functions involving one, two, or three
control components, the candidate's eigenvalues are respectively `1/3`, `1/6`,
and `0`; the matched whole-command control has `37/210` for every nonconstant
function. Their multiplicities are 10, 17, and 8. Thus the candidate retains
individual-component correlations more strongly, pair interactions slightly
less, and triple interactions not at all across one ordinary transition. This
is a precise difference despite identical complete-command run lengths. Special
taps multiply these eigenvalues by `11/12`; independent suffix starts truncate
the dependence. [All basis checks pass](kernel-spectrum.json) without emulation.
This explanation was added after registration and changes no policy or gate.

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

## Native qualification and registered development

The implementation at `2fb37f9c` passes exhaustive transition checks, 137 NES
library tests and the evaluator test, strict all-feature Clippy, and 23 Python
evaluation-runner tests. Nondefault policy identities must match during replay;
every suffix starts independently of previous expansions.

| Check | Result | Known auxiliary frames |
| --- | --- | --- |
| Q01, two historical defaults on msr1 | Exact stream, campaign and checkpoint bytes; full replay | 1,045,660 |
| Q02, both policies × both games × two buffer sizes | All eight replay; every buffer pair has identical artifacts | 826,696 |
| QX01, six reused fixtures on ms02 | Exact ARM post-header event streams, semantic results and local replay | 1,459,008 |

These are correctness checks on a reused seed, not performance samples. The
short correlation cases cap trajectories at 64 actions. Full replay work is
inferred from admitted work; setup, reconstruction and unadmitted work remain
additional unknowns. The [updated ledger](ledger-after-qualification.json)
contains **371,963,017 performance-search frames and 44,118,607 known auxiliary
frames** in the renewed block. Compressed raw panel records and their analyses
are stored beside each registration. The Q02 dispatch label accidentally used
`HEAD-registered-q02`; QX01 records its exact binding to commit `676ee62e` and the
registered content hash, without rewriting the original result.

[D01](d01-registration.json) freezes four fresh development seeds and three arms:
ordinary independent commands, component refresh, and matched whole-command
persistence. It tests first energy-tank acquisition at a 50M-frame horizon,
using the already calibrated ordinary selector and identical other settings.
Within every triplet, all arms run sequentially on one host and CPU set; two
triplets run concurrently across msr1 and ms02. Arm order is fixed in advance.
The source archive, architecture-specific binaries, all identities, seeds,
20-minute search limits, finishing limits, process memory and output bounds
are registered before dispatch.

The first wave contains seeds 0 and 1. Only if both comparison gates remain
attainable does the second wave launch seeds 2 and 3. Each comparison requires
three strict wins, at least 15% lower mean restricted cost, and candidate
CPU/elapsed totals at most 1.25 times that control. Both-censored pairs tie.
These are compute-allocation rules, not statistical significance. The maximum
is 600M nominal search frames plus the bounded in-flight drain; known auxiliary
replay has a separate 5M allowance within the remaining block balance.

At registration, no performance outcome has been inspected. Passing both gates
would earn independent confirmation and same-policy MM2 transfer; neither a
qualification nor a precursor win completes the original boss/Wily goal.
