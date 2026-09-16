# Step 5: more than one champion per slot

Base: the step 4 merge commit, with the whole groundwork sequence validated.
Refresh line numbers against that base before editing.

## What is wrong

A depth-0 slot keeps one holder, chosen by `preference_cmp`. Metroid's
preference puts missiles before health, so at one slot a state with 10
missiles and 20 health beats one with 5 missiles and 200 health, even when
only the second can survive the route out. One comparator decides which
state at a place is worth keeping, and the states it discards are the ones a
different route needs.

## What it becomes

A workload declares a small fixed ordered list of preferences instead of one.
A slot retains the union of the champions under those comparators, so Metroid
keeps at most two holders. Every candidate is judged against every preference
whatever its parent came from; that sharing is the point, since either route
can improve either champion. One entry can hold both memberships, and it is
stored once.

Grouping, progress ordering, terminal classification, the input draw,
durations, novelty and objective detection do not change. The searcher handles
stable preference identities and comparisons; the workload vocabulary stays in
the workload. This is several champions at one place, not several independent
searches: class selection and progress use the same policy.

| Piece | Today | After |
|---|---|---|
| preference | one `preference_cmp` | a fixed ordered list; a one-preference workload behaves as today |
| slot contents | one holder | the union of champions, compared by preference, then path cost, then a stable tie-break |
| candidate judging | against the parent's preference | against every preference |
| slot draw | the cheapest-member weighting | equal seeded probability over preferences, then that preference's champion |
| continuation trigger | a replacement holder | either champion improving, with the champion and parent generation on the pending entry |
| record | selector draw | plus the portfolio identity, the preference order and the selected preference |
| accounting | per-slot totals | plus exclusive and shared holders, selections and replacements per preference, cross-preference improvements, and portfolio memory |

Losing one membership must not drop an entry that still serves another
preference or is pinned by an admitted reservation. Eviction, a missing
membership and deterministic replenishment each need a written rule; memory
pressure must leave no dangling champion and no retry loop.

Continuations keep the reservation share, the tail bound, the stale-work bound
and the accounting isolation they have after step 3. Identical work for a
parent serving both preferences is coalesced, so preferences do not double the
continuation entitlement. A replayed tail can land somewhere other than its
recorded destination; judge the result that arrives.

Membership indexes, extra retained states, pending work and retained history
are charged to the same logical budget. Report resident set size beside it.
A shared result counts once as an objective and once as a new place. A
same-slot improvement under one preference does not reset spatial novelty
energy for the other.

## Steps

1. Extend the preference contract with the ordered comparator list. Pick the
   smallest shape that fits the trait as step 4 leaves it.
2. Retain the union at each slot and track membership separately from storage.
3. Judge every candidate against every preference at ordered admission.
4. Insert the preference choice at the final slot-holder draw, after the
   existing hierarchy has already chosen the place. Read the selector as it
   stands first and write down any change to the sampling rules.
5. Audit the continuation queue against the new trigger.
6. Record the portfolio identity, the preference order and the selected
   preference. Reject an incompatible recording through the existing policy and
   schema checks.
7. Add the bounded diagnostics above.
8. Rewrite the retention paragraphs of `dissonance/searcher/README.md` and the
   Metroid preference paragraphs of `workloads/nes/README.md`.

## Tests

Use a target-neutral fixture with two bottlenecks that need opposite resource
trades. Cover: both champions survive and each is drawn; an outcome from one
improves the other; shared champions; ties; membership loss; eviction against
reservation pins; stale continuation work; propagation from both champions;
repeated improvement without an unbounded queue.

Then a checkpointed multi-worker campaign with exact serial replay, and replay
rejecting corrupted preference and continuation evidence. Run a one-preference
configuration against the frozen step 4 build and compare decisions and
results; compare normalized semantics rather than bytes if the record format
moved.

## Arms

Freeze the manifests, seeds, build identity, budgets and endpoints first. Same
memory, workers, input draw, continuation entitlement and campaign budget for
every arm; the portfolio gets no budget per preference.

| Arm | Retention and selection |
|---|---|
| A | one missile-first champion, the step 4 build |
| B | one health-first champion |
| C | two champions under missile-first alone, a capacity control |
| D | missile-first and health-first together |

C is what separates keeping a second state from keeping a strategically
different one. Paired seeds, and do not assume the arms consume the same draws
once they diverge.

## Checks after merge

Correctness checks, then the SMB regression and the quick panel as regression
checks. Then fresh-start Metroid as the experiment, with the long panel on both
manifests. A snapshot start may explain a bottleneck; it is not capability
evidence.

Judge it the way the sequencing README says: film and map images first, then
numbers. Report verified first-objective work and solves, progress at matched
executions and at matched execution work, which resource trades are retained at
useful places, the draw share and cross-improvement count per preference,
continuation effectiveness, throughput and memory. Keep the failures and the
incomplete runs. An equipment union across branches is not a trajectory.

Unlike steps 1 to 4, this one is an experiment and can be rejected. Use the
pilot seeds to diagnose and freeze the candidate for a separate confirmation
set. Recommend it as a default only if it produces repeatable progress against
both single-preference arms and against the capacity control, at an acceptable
measured cost and with no unexplained regression. Say so plainly when the seed
count is small. A mixed or negative result is an answer.

Pareto frontiers, adaptive allocation across preferences, larger portfolios and
new input draws are later experiments. Do not grow this one during
implementation.

## Where this came from

Antithesis separates input generation from state judgement and describes running
two objective functions at once, one of them rewarding readiness to clear four
lines at once. The posts do not say how the two are shared or scheduled.

- https://antithesis.com/blog/2025/gradius/
- https://antithesis.com/blog/2025/metroid/
- https://antithesis.com/blog/2026/tetris-quest/
