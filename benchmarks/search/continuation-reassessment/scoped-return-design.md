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

## Implemented source hypothesis

The explicit scoped-return selectors implement a bounded redirection of the
existing group-walk cell proposal:
with probability one half, choose a strictly higher-progress member of that same
retention slot if it has the same known scope, no worse known resources, and is
eligible under the same active/action-limit/window/exhaustion rules. Otherwise
keep the original proposed parent. With the qualified two-member policies this
examines at most one alternate; it needs no global oracle, progress cache or
cross-scope ranking. The underlying room/cell proposal law stays fixed.

The insertion point is after the existing eligible recency window is formed.
The unchanged uniform quarter bypasses it. A redirect cannot revive an entry
excluded from that window; the original all-exhausted reset remains intact. Thus
the old failed exhaustion-ablation gate must not be silently reopened. Keep the
suffix vocabulary, duration distribution, retention and progress projection fixed.
No recorded job seed, action tail or selected checkpoint becomes fresh-search input.

## What can be proved before running

Condition on a group-walk proposal falling in an unchanged eligible two-member
slot, with both members inside the offered window, containing an ordinary member
and a higher-progress member with no resource loss. Let q be the original
probability of choosing the latter conditional on that proposal event. An independent
redirect coin with probability epsilon gives

```
q_new = q + epsilon * (1 - q)
epsilon = 1/2  =>  q_new = (1 + q) / 2
```

This is a one-step conditional allocation identity. In an explicitly stationary
independent-trial model with eligible group-walk slot-offer probability s, expected draws to that
member change from 1/(s*q) to 1/(s*q_new). Neither s nor q is estimated by the
observed gaps above. Real eligibility, populations and future outcomes change,
so the model supplies no campaign-level bound or boss-defeat guarantee.

Use the production sampler in a small finite fixture: place a qualified alternative
behind an ordinary cost advantage, check the allocation identity and complete
eligibility constraints, and include an adverse world whose only successful
continuation needs the ordinary member. There is no stochastic-dominance claim
over futures. Test unknown scope/resources, ties, failed qualification, inactive
and exhausted members, action ceilings and historical defaults.

The source contract uses identifiers
`room_cell_uniform_128_energy_progress_cheapest_scoped_return_control_v1:<thresholds>`
and `..._scoped_return_half_v1:<thresholds>`. Both draw exactly one extra
`below(2)` coin after every group-walk cell proposal, including no-op windows;
heads means nonzero. Control ignores it. Uniform draws consume none. Old
identifiers retain their original behavior. The comparison couples RNG from an
identical history; subsequent parent/history divergence can change later draws.

`archive_scoped_return_tests.rs` exercises the production sampler across 512
fixed fixture seeds with four cost-ranked filler slots. It checks each actual
choice and next RNG value against the baseline proposal plus the shared coin,
then enumerates both coin outcomes over that finite proposal population. Its
adverse world assigns success only to the ordinary member: that member retains
positive probability, but its conditional mass halves. Separate production-walk
checks cover inactive/nonresident/action-limited/exhausted alternatives, reset
application and recency exclusion. Qualification checks reject unknown progress,
unknown resources, scope mismatches, ties and resource tradeoffs. These fixtures
supply no gameplay input or efficacy seeds.

The generic campaign suite checks the new recorded policies, continuation draws,
1/4 workers, both result buffering modes, memory pressure and full replay. Its
key has no scoped progress, so native qualification must still demonstrate an
actual changed parent and complete replay with qualified alternatives. The
bounded native caller accepts only this family at `3,6,12,2`, requires the
progress feature for both new policies, and preserves omission defaults.

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

## Source qualification

The complete generic release suite passes 172 tests plus the interface check;
strict all-feature/all-target searcher Clippy passes. The challenge caller passes
7 tests with the progress feature and 7 with defaults; its feature-build strict
Clippy passes. The general correctness, deterministic execution, external input,
compatibility, semantic ownership and test-strength lenses in REVIEWING.md were
applied. No unsafe code or its invariants changed. SR01 subsequently qualifies native activation and complete replay in all four
arms; see sr01-results.md. Efficacy remains unmeasured; passing implementation
checks does not close the research goal.
