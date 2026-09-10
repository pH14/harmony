# Next question: allocating returns to a preserved progress alternative

PQ01 changes the diagnosis. In the fixed saved PG02 run, the progress rule leaves
a same-scope HP94 state at the original health79/missiles0; ordinary's best cached
state is HP133 at those resources. Lower-HP saved states receive 1,403/806 executed
parent selections respectively. Thus a claim that all such states are discarded
or never revisited is false for these traces. This is conditional proxy evidence,
not a successful efficacy screen or a result against the unrun capacity control.

A specific recorded return is informative: progress ID2014 has HP95 and health79,
is created at job5436, and is first used at job7362. That 123-frame job produces
ID2572 at HP94 and the same health. ID2572's first selection is job9979. The observed
gaps are 1,926 and 2,617 jobs. They are not continuous-eligibility measurements:
cached reconstruction anchors can be inactive, and the final snapshot set is a
selected sample of history. They motivate a return-allocation question, not an
estimated opportunity rate or a guaranteed speedup.

## Minimal hypothesis to falsify in source first

Investigate a bounded redirection of the existing selector's proposed parent:
with probability one half, choose a strictly higher-progress member of that same
retention slot if it has the same known scope, no worse known resources, and is
eligible under the same active/action-limit/window/exhaustion rules. Otherwise
keep the original proposed parent. With the qualified two-member policies this
examines at most one alternate; it needs no global oracle, progress cache or
cross-scope ranking. The underlying room/cell proposal law stays fixed.

This is not yet an implemented policy or a native allocation. Audit the actual
selector's admissibility boundaries before choosing the insertion point. If a
redirect would revive an exhausted or otherwise ineligible entry, reject it;
the old failed exhaustion-ablation gate must not be silently reopened. Keep the
suffix vocabulary, duration distribution, retention and progress projection fixed.
No recorded job seed, action tail or selected checkpoint becomes fresh-search input.

## What can be proved before running

Condition on an unchanged eligible two-member slot containing an ordinary member
and a higher-progress member with no resource loss. Let q be the original
probability of choosing the latter given that slot was offered. An independent
redirect coin with probability epsilon gives

```
q_new = q + epsilon * (1 - q)
epsilon = 1/2  =>  q_new = (1 + q) / 2
```

This is a one-step conditional allocation identity. In an explicitly stationary
independent-trial model with slot-offer probability s, expected draws to that
member change from 1/(s*q) to 1/(s*q_new). Neither s nor q is estimated by the
observed gaps above. Real eligibility, populations and future outcomes change,
so the model supplies no campaign-level bound or boss-defeat guarantee.

Use the production sampler in a small finite fixture: place a qualified alternative
behind an ordinary cost advantage, check the allocation identity and complete
eligibility constraints, and include an adverse world whose only successful
continuation needs the ordinary member. There is no stochastic-dominance claim
over futures. Test unknown scope/resources, ties, failed qualification, inactive
and exhausted members, action ceilings and historical defaults.

The extra coin must have an explicit replayable RNG contract. Compare against a
control with the same proposal/RNG consumption but no redirect, isolating the
return rule from changes to mutation tapes. Finalize the exact
source contract and identity before a prospective native registration.

## What would earn an experiment

A source-level falsifier and replay qualification must pass first. Any subsequent
conditional screen needs fresh frozen seeds, the same 1M-frame horizon, complete
physical accounting and living named defeat as its endpoint. Include the
unchanged progress-retention comparison to isolate the return change, ordinary
retention, and a suitable scoped capacity control; do not omit the latter based
on the incomplete PG02 results. Fix the full comparison and allocation criteria
before observing outcomes. No longer horizon follows automatically from PQ01.

Any pass still requires fresh-search development, independent confirmation,
MM2 evaluation and untouched validation. The full goal remains unachieved.
