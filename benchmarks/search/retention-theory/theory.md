# What archive retention can and cannot guarantee

This is a research contract, not a proof of the NES searcher. The finite models
are executable in `dissonance/searcher/src/search/archive_abstraction_tests.rs`.
They use the production archive admission rules, while their transition systems
are deliberately small and completely enumerated.

## Define the claim before choosing a heuristic

Let a deterministic state include the emulator, adapter caches, pending input,
and remaining task budget. An action may be a held controller chord. Its
transition emits `(next_state, actual_cost, task_events, terminal_status)`;
events inside a held action count. The archive maps states to cells through
`phi`. A cell representative is a restart state, not a proof that the other
states in the cell are interchangeable.

A sufficient condition for replacing `s` by `r` without changing any allowed
task trace is an equivalence relation `~` satisfying:

* initial task labels and remaining budgets agree;
* for every allowed action, costs, events, and terminal status agree;
* successor states are again related.

Induction on suffix length establishes equality of every finite allowed trace.
The base case compares initial labels. The induction step uses the matching
transition output and applies the hypothesis to related successors. This gives
reachability preservation, not equality of search streams or finite-budget
discovery probabilities. Different prefix costs must be included in remaining
budget; the actual searcher's within-group cost alone does not establish that.

For one-way replacement, a cost-respecting simulation can suffice: every
original continuation must have a matching, no-more-expensive continuation
from the replacement. Matching the same action sequence is a strong sufficient
condition, not a necessary condition for task reachability. Resource ordering
is neither of these relations unless transition monotonicity is separately
established. More health does not imply the same velocity, enemy phase, door
state, or response to the next button press.

This follows the distinctions in [state abstraction theory](https://thomasjwalsh.net/pub/aima06Towards.pdf)
and [bisimulation metrics](https://arxiv.org/abs/1207.4114). We do not import an
MDP approximation bound without checking its reward, horizon and transition
assumptions against held actions and emulator resets.

## Counterexamples are cheaper than claims of equivalence

For a finite deterministic model, explore the product graph `(s,r)`. An edge
whose cost/event/terminal output differs supplies a distinguishing suffix.
If the reachable product graph is exhausted without a difference, equivalence
holds for that finite model and action vocabulary. It does not hold for the
NES merely because the model passed. Partition refinement repeatedly splits
cells by output and successor-cell signatures until stable. It is an exact
finite analogue of the refinement direction, inspired by
[CEGAR](https://www.cs.cmu.edu/~emc/papers/Conference%20Papers/Counterexample-guided%20Abstraction%20Refinement.pdf).

On NES states, a replayed distinguishing suffix is a valid counterexample to
the particular equivalence claim. A finite set of matching sampled suffixes
is only distribution-specific evidence. With zero mismatches in `n` independent
draws from a fixed suffix distribution, the one-sided 95% upper bound on that
distribution's mismatch probability is `1 - 0.05^(1/n)`. At n=100 it is about
2.95%. Shared samples across pairs, adaptive choice of states, and multiple
claims need separate treatment. Rare decisive suffixes can remain invisible.

## A Pareto front is a resource statement

One slot can contain resource vectors `(10,0)`, `(5,5)`, `(0,10)`. All three are
nondominated. If an exit needs at least three of both resources, only the middle
state succeeds. Keeping the two coordinate extremes loses the exit even when
resources are genuinely monotone in the transition system. This is a stricter
counterexample than a hidden-physics alias: two extremes do not preserve all
monotone threshold tasks. A full resource front still cannot establish
behavioral equivalence when unrepresented state differs.

[MOME](https://arxiv.org/abs/2202.03057) motivates bounded local Pareto sets, but
does not make the production two-extreme heuristic a task-preservation theorem.
Its usefulness here requires evidence under the same total archive memory.

## Coverage and discovery probability are different objectives

For a frozen probe suite, let `C_s` be the set of useful outcomes exposed by
state s, and let outcome weights be nonnegative and fixed. Then

`F(R) = sum_j w_j * 1[j belongs to union(C_s for s in R)]`

is monotone submodular. Under a cardinality limit K, greedy marginal coverage
has the usual `1 - 1/e` guarantee relative to that frozen coverage objective
([Nemhauser, Wolsey and Fisher](https://link.springer.com/article/10.1007/BF01588971)).
This excludes unknown future outcomes, adaptive probes, unequal snapshot cost,
and histories whose value depends on later archive combinations. Optimizing
the probe suite is not a guarantee about unseen game progress.

The implemented R02 resource proxy is the discrete two-dimensional hypervolume
subset problem with an inclusive zero origin. This connection is established
in the [two-dimensional subset-selection literature](https://eden.dei.uc.pt/~paquete/HSSP/).
The [multi-objective archiving review](https://arxiv.org/abs/2303.09685) also
distinguishes storing good solutions from using the archive to drive search.
R02 solves only the tiny current competition exactly; it does not claim a new
hypervolume algorithm, streaming limit optimality, or a bound on NES discovery.

An executable reverse counterexample makes that limitation concrete. In arrival
order A=(0,10), B=(4,3), C=(10,0), D=(4,10), R02 retains A,B over C because
their threshold union is 27, versus 26 for B,C and 21 for A,C. D then dominates
both survivors, leaving coverage 55. The forgotten C would complement D to
cover 61. Coordinate extremes preserve C and therefore preserve an exit needing
resource 0 >= 10, while R02 loses it. Neither heuristic uniformly preserves
useful futures, even in these monotone resource worlds. The formal tests check
both directions; the candidate was not evaluated only on a world favoring it.

Even with known outcomes, finite search probability need not be monotone in
retained states. Under independent uniform parent draws, one state with success
probability p per attempt gives `1-(1-p)^B` success after B attempts. Adding a
second state with zero success probability changes this to `1-(1-p/2)^B`, which
is strictly smaller for p>0, B>0. More reachable futures can therefore help the
coverage objective while hurting a specific finite-budget target. The real
selector is hierarchical and adaptive, so these numbers are an explanatory
model, not fitted predictions for Metroid.

The production selector has a one-quarter uniform-active-entry branch. If an
entry stays active for L reservations, with at most N active entries at every
one, its chance of never being selected is at most `(1-1/(4*N))^L`, under the
ideal random-draw model. This says little when N is large and retention lifetime
is short. It also excludes removed, exhausted, or horizon-ineligible entries;
deterministic seeded execution alone supplies no probabilistic guarantee.
Do not mistake a positive asymptotic probability for adequate finite exposure.

Nor is zero parent selections proof of zero executed continuations. A worker
can execute A then B in one suffix job; admission retains both boundaries,
but selection bookkeeping credits only the original job parent. The new
`zero_parent_selections_does_not_mean_no_executed_continuation` fixture runs
the production rollout and coordinator and confirms that A's retained state
has zero selected/productive counts despite an executed, retained descendant.
Therefore `removed_never_selected` and `removed_never_productive` do not count
all unexplored states or all states without useful outgoing work. They describe
job-parent allocation. A scheduler intervention needs either a distinguishing
probe or exposure accounting that also covers continuation within birth jobs.

There is a second timing distinction: normal parent selections are credited
after ordered job admission. A pending job may already have executed from a
state when an earlier admitted job removes that state. The removal-time census
can therefore record zero selections even though the later admitted job raises
that inactive entry's selection count. A second production-coordinator fixture
exercises that exact order. Treat the lifecycle census as removal-time admitted
parent accounting, not a complete measure of physical exploration starvation.

## Predictions that decide the next experiment

Observation validity precedes these abstraction claims. The parallel research
effort found a real transient BCD health underflow: a dying endpoint appeared
to have 9800 health before the next frame cleared it. A mathematically optimal
resource ranking over that observation would preserve a false advantage. The
versioned terminal correction imported from `e59a953a` supplies the missing
validity predicate while preserving raw evidence. Corrected terminal semantics
must be identical in both arms of subsequent key experiments. This is a
correctness premise, not evidence that the finer key improves boss attainment.

1. Identity refinement should first reduce replayed continuation disagreements
   on held-out suffixes for states split by that refinement. If it only grows
   cell count without separating useful futures, do not spend a long run on it.
2. Resource retention should preserve additional useful continuations before a
   fresh campaign is expected to benefit. Test the middle of fronts as well as
   coordinate extremes; report useful futures lost by either choice.
3. If useful alternatives survive but get fewer attempts per unit work, test
   allocation separately. Measure active lifetime, actual selection exposure,
   reconstruction cost and useful suffix yield, not only total entries.
4. Qualify an end-to-end candidate with matched work/memory and fresh seeds.
   Progress from hand-selected diagnostic starts is explanatory evidence only.

The intended loop is: claim, assumptions, smallest counterexample, distinguishing
probe, mechanism-specific prediction, bounded experiment, then fresh validation.
The theory pass ends after two hours even if no useful guarantee was found.

## R03: a best representative and a job-ranked sample

P03 supplies new counterexamples at equal health, missiles, equipment and
capacity: 14/16 pairs have distinguishing local exits or survival, and the
refined key separates only six of those pairs. It does not identify a feature
that reliably ranks their unknown futures. A bounded alternative is to retain
one ordinary quality representative and one representative selected by a
separate ordering over job cohorts.

For a fixed stream of nonduplicate candidates in one slot, let Q order opaque
preference, lower group cost, then earlier stream arrival. Stable retained-entry
IDs implement that last comparison: every current entry predates the fresh
candidate, and retained IDs preserve their arrival order. Rejected candidates
need no persistent ID for this interpretation. Give every candidate
from creation execution j the same rank r(j). Keep the Q-maximum candidate
and the Q-maximum candidate in the minimum-rank cohort. These may coincide.
Updating from the current winners plus one candidate preserves both extrema
of the entire seen stream by induction: max over a union and min over a union
can each be computed from their previous winner and the new value. No cohort
count, per-entry random state, or extra archive dimension is required. This
claim assumes no external eviction/import boundary and stable quality values.
The unchanged exact-input deduplication path is outside this candidate stream;
repeatedly offering the same already-known input is not a new sampled cohort.
The tests check every prefix of a fixed stream and an explicit within-cohort
counterexample where the better-ranked resource state loses a useful future.

Under an ideal independent continuous rank assignment to n fixed cohorts,
each cohort is the minimum with probability 1/n. If m cohorts have a useful
Q-best member, the sampled representative is useful with probability m/n;
the additional global Q-best representative cannot reduce retained coverage.
This is a model calculation, not a probability guarantee for the implementation.
The implementation uses a fixed bijective integer mixer on recorded execution
numbers, and the search adaptively generates later candidates. Neither
independent random ranks nor an exogenous stream is established in live search.
It also samples cohort representatives, not all physical states. Keeping a
sampled state does not ensure the selector spends enough work on its future.

The construction uses the minimum-rank idea in
[Cohen and Kaplan's bottom-k sketches](https://www.cs.tau.ac.il/~haimk/papers/p225-cohen.pdf).
Hash-family assumptions matter for probabilistic claims, as emphasized by
[Thorup's analysis](https://arxiv.org/abs/1303.5479); a fast deterministic mixer
is not substituted into those theorems without their hypotheses. R03's exact
extrema invariant and its empirical search effect are separate claims.

## Chained progress and the quantity a comparison estimates

Let P_j(pi, s) be the searched and replayed prefix carried into stage j by
policy pi on seed s. Let T_j(pi, s, p) be admitted work until a verified stage
victory from fixed prefix p, with infinity denoting no victory. The fresh-chain
stage cost is T_j(pi, s, P_j(pi, s)): earlier retention choices also change the
later starting emulator state. Its route, resources and timing are consequences
of the intervention, not nuisance values that can be assumed equal.

For two policies A and B, comparing T_j(A, s, P_j(A, s)) with
T_j(B, s, P_j(B, s)) measures their end-to-end stage outcomes. Holding the
prefix fixed instead compares T_j(A, s, p) with T_j(B, s, p), a conditional
effect of retention at that origin. These are different questions. The second
can diagnose a mechanism but cannot replace a fresh-chain result. Selecting p
because A succeeded there also prevents treating that one contrast as an
unselected population estimate. H01 reuses a completed candidate result for
this conditional question and makes the selection explicit.

With fixed stage limits b_j, chain success is the intersection of events
T_j(pi, s, P_j(pi, s)) <= b_j, plus qualified bridges and the total budget.
The probability of this intersection is not the product of unconditional
stage success rates: the carried prefixes couple the events. Independent
fresh-seed replication evaluates the complete chain, including that coupling.

Wall limits create a further distinction. If a control stops after c frames
without success, the observation is T_control > c, not T_control = infinity.
A candidate victory at t > c therefore gives no ordering of their victory
costs. This is exactly J01 Heat: 66,633,523 candidate frames and only 40,507,046
observed control frames. For a 20% candidate frame reduction, the required
comparison is 5*t <= 4*T_control. Completing a control budget of 85M without
success would establish that conditional inequality for this candidate; an
earlier control victory might refute it. H01 also withholds its allocation
gate on wall censoring, rather than silently substituting elapsed time for
completed work. Report setup, replay and bridge work separately from admitted
search work; neither metric alone is the full cost of a fresh chain.

## Context maxima are a mechanism claim, not a discovery theorem

P04 supplies a concrete refinement candidate: a coarse motion descriptor
separates six useful competitor pairs still merged by the finer position key.
It also contains a counterexample where all inspected motion bytes agree but
the futures differ. The [context-maxima construction](motion-retention-design.md)
therefore has two separate obligations: an exact fixed-stream retention
invariant, checked with finite positive and adverse examples, and a matched
adaptive-search comparison against a top-two-quality capacity control.

The active retained set must also be distinguished from physical snapshot
residency. Historical ancestors can own snapshots needed for reconstruction
without participating in retention. Counting checkpoint payloads per cell can
falsely report capacity violations and same-context alternatives. R04 exposed
this measurement error; R04b counts the actual active keys. Correctly measuring
two distinct contexts establishes only the implemented partition choice, not
two useful futures or sufficient continuation exposure.

J03 illustrates the separate end-to-end obligation. Job sampling improved Metal
on the new seed, yet ordinary retention completed four stages while sampling
failed Heat at the full shared ceiling. An early-stage improvement and a
favorable selected-prefix diagnostic do not imply better chained discovery.
That prospective failure closes this family's escalation gate for the tranche;
it does not falsify the fixed-stream extrema invariant or prove eventual
success impossible.

The [bounded literature update](literature-update.md) adds a further executable
obligation for predictive descriptors: two states can have identical average
event features under one action mixture while requiring opposite actions.
Action-conditioned evidence is therefore necessary for a claim that survives
changes in the exploration mixture; exact successor-model theorems require
stronger conditions than a fitted short-horizon average. The new finite fixture
passes and changes no running experiment.
