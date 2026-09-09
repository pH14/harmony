# Predictive retention: a bounded literature update

This September 9 update supplements the initial literature pass while the
frozen R04b experiment runs. It changes no running policy, seed or gate.

**Keep action-conditioned evidence.** Lehnert and Littman's linear successor
feature model connects predictive representations to bisimulation under
specific conditions: one-hot abstract states, exact reward prediction and the
action-conditioned fixed-point equations holding for every state/action. Their
real-valued reward-predictive representations need not encode bisimulation.
These distinctions matter more here than borrowing the method's name.
[Primary paper, Theorem 2 and section 4.2](https://arxiv.org/pdf/1901.11437).

Our additional finite counterexample makes the practical failure precise.
States u and v have two actions. At u, action0 succeeds and action1 fails;
at v, those outcomes reverse. Both enter absorbing states afterward. A uniform
action mixture gives identical expected future event features (success
probability one-half at every sufficiently long horizon). Yet action0 alone
distinguishes them, and changing its probability to three-quarters yields
success probabilities three-quarters and one-quarter. Thus a policy-averaged
event fingerprint can hide an important alternative. This does not contradict
the paper's stronger action-conditioned conditions. The executable fixture is
`equal_policy_averaged_event_features_can_hide_opposite_action_futures` in the
existing abstraction tests; partition refinement separates the states.

**Keep action history attributable to the state actually continued.**
Intelligent Go-Explore (2024) separately changes state selection, action choice
and archive filtering, and stores per-state tried-action history. Its ablations
find the importance of those components varies across text environments.
[Primary paper, sections 3 and 5](https://arxiv.org/html/2405.15143v2).
For Harmony, the transferable hypothesis is bounded recording of attempted
continuations at exact physical origins. Our parent-credit counterexamples
show why a selected-parent counter cannot supply that history. Any such change
needs its own measurement gate and belongs with
[#283](https://github.com/pH14/harmony/issues/283); the current retention test
does not add a foundation-model policy or change action selection.

**Do not infer feasible exits from empty map cells.** TopoExplore's July 2026
preliminary report adds entrance-gated topological selection. It reports a
positive controlled MiniGrid comparison, but its single-seed Montezuma sweep
degrades without a wall mask: unreachable occupancy pockets attract selection.
The claimed mechanism depends on environment-supplied traversability in its
positive settings. [Primary report, sections 2–4 and 7](https://arxiv.org/html/2607.09971v1).
This supports testing reachability evidence before proposing a frontier bonus
for Metroid. Our replayed living exits establish local feasibility for specific
states and suffixes; they do not establish a complete map or a navigability
model. No topology bonus is justified by the current occupancy census.

The resulting next hypothesis, if future work warrants it, is a bounded
action-conditioned continuation signature: record the same small probe set
from competing states, retain observed success/survival distinctions, and
measure prediction on newly specified probes before fresh-search evaluation.
Specify the probe distribution and horizon, account for physical probe work
and storage, include an adversarial action-mixture example, and distinguish
deterministic reconstruction from generalization. A signature that predicts
sampled short futures still needs a matched discovery comparison; it does not
inherit an all-actions, all-horizons bisimulation theorem. This is a follow-up
design obligation, not authorization for another long run after a failed gate.
