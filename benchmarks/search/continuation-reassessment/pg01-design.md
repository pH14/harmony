# PG01: preserve an ordinary anchor and qualified progress

PC01 supplies a concrete reason to revisit retention: one actually rejected
state has better new-damage continuations at unchanged player resources. It does
not establish that lower HP dominates other complete states or that a particular
HP discretization will improve fresh search. The candidate therefore preserves
the ordinary representative and uses at most one additional member. A scoped
capacity control shares its eligibility rules, isolating the alternative's
ranking from merely allowing a second eligible state.

## Exact local rule

For the current slot members plus one viable, nonduplicate candidate, let Q be
the existing order: workload preference, then lower time in the coarsest group,
then earlier stable admission ID. Choose ordinary anchor r = argmax Q. Keys may
optionally provide P(s) = (scope, value), and the existing two resource axes R(s).
Scopes have equality semantics; values may be ordered only within one scope.

```
E = {s : P(s), P(r), R(s), R(r) are available,
         scope(s) = scope(r), value(s) > value(r),
         R(s)[0] >= R(r)[0], R(s)[1] >= R(r)[1]}

candidate:        a = argmax E (value(s), Q(s))
capacity control: a = argmax E Q(s)

proposed survivors = {r} if E is empty, otherwise {r, a}
```

Both alternatives must improve the progress proxy strictly; the control also
uses that eligibility test. Thus it controls capacity and eligibility, not all
use of progress information. Equal proxy values use Q. Unknown context never
becomes zero progress, and resource improvements cannot be purchased with a
loss on the other resource axis. An unavailable primary has no alternate.
The ordinary preference itself remains unchanged.

The existing admission lifecycle applies this proposal when the new candidate
belongs to the survivor set; otherwise the offer is rejected. Exact-input
deduplication and global memory/population eviction remain separate. These two
policies apply only to workloads whose ordinary slot capacity is one; other
workloads use their existing ordinary rule. Imports, external evictions and
historically discarded states are outside this offered-set claim.

## What is proved and what remains empirical

For each eligible local offer, the proposed set has at most two members, contains
the ordinary anchor, and any alternate has a matching known scope, higher proxy
value and no worse supplied resources. The production-archive tests check the
offered-set oracle over all 24 permutations of a finite stream for both rules.
They distinguish the candidate from its capacity control and exercise missing
context, unknown resources, mismatched scopes, equal values and resource tradeoffs.

The real exported PC01 inputs and keys also exercise the production archive:
the incumbent's local input has 206 frames and the candidate's 296, with equal
ordinary preference. Ordinary retention rejects the candidate; both new policies
keep it alongside the ordinary anchor. Its qualified values are 114 and 117,
corresponding to the observed HP140 and137. This is a source-level replay of the
local admission rule, not another emulator rollout or reconstruction of all
preceding search decisions.

None of these invariants proves beneficial futures. A planted finite world puts
its only future success behind an eligible state with intermediate progress that
the candidate discards. Likewise, retaining an ordinary anchor per offered slot
does not preserve the entire ordinary search trajectory: selection probability,
future offers, global eviction and actual memory occupancy can change. No global
Pareto, discarded-history optimum or adaptive-search dominance is claimed.

The next empirical question is whether preserving such alternatives compounds
into **living named boss defeats**, measured against ordinary retention and the
scoped capacity control at matched physical-work and memory budgets. More retained
HP levels, admissions, local damage or early deaths cannot substitute for that
outcome. A positive conditional result would still require fresh-search development,
independent confirmation and both-game transfer before untouched validation.

## Explicit Metroid projection

`metroid-retention-progress` is an opt-in feature that also enables the existing
motion metadata and endpoint observation schema. All comparison arms must use the
same build, including the ordinary arm. It adds an optional key field without
changing any grouping coordinate, ordinary preference, selector, action law or
snapshot observation field. Full-key equality and ordering do include the new
metadata, so cross-feature splice-donor and resume behavior need not match.
Ordinary group-based key counts keep their existing group identity. The initial
qualification and comparison must use `alphabet_only` in every arm; do not
attribute a cross-feature action-mixture comparison solely to retention.

The target first requires a live, nonterminal state. Its same-boundary decoder
must find exactly one classified undefeated Kraid/Ridley slot with HP other than
255. Scope packs eight exact bytes: area, slot offset, data index, qualified
attributes, equipment bits, energy-tank count, missile capacity and boss flags.
This also prevents the inherited item/tank counts from hiding capability
tradeoffs. Equal scopes have identical values for these four capability fields;
the existing axes compare current health and missile stock separately. Value is 254 minus snapshot-local HP. Normal and saved
hit attributes identify the same scope; current hit status supplies no rank.
HP0 is not a named defeat flag. Missing, ambiguous, unavailable or already-defeated
boss state produces None. Gameplay mode3 is a classification requirement, not
a general aliveness predicate.

This projection reads the already captured WRAM and same-boundary cartridge RAM;
it does not consume observer totals, parent outcomes, future continuations or
solution inputs. The cartridge read adds host work, with no emulator frames.
Both arms pay the same metadata cost. Larger keys are included by the archive's
existing size-based memory charges; alternate snapshots share the same budget.
Actual process memory and work must still be measured. The observer remains
separate and its interval output does not guide policy.

The generic policy IDs are `resource_guarded_progress_2_v1` and
`resource_guarded_progress_quality_control_2_v1`. Metroid's feature records a new
key policy, campaign format and result-digest identity; historical snapshots keep
their unchanged schema. Feature omission preserves historical policy identities.
Old campaign streams cannot be silently relabeled into the new key policy. MM2
currently provides no new progress projection, so transfer is unimplemented and
unmeasured, not supplied by the generic API alone.

## Next gate

`metroid-archive-challenge` now accepts an optional explicit `slot_retention`
and `expected_retention_progress`. It rejects unsupported feature/policy requests
before I/O; qualified root replays check the exact descriptor and verify that
reading it changes neither full snapshot nor physical clock. Its existing
alphabet-only campaign, complete replay, witness checks and hard bounds remain.
The direct helper counters do not expose every generic engine constructor or
unadmitted frame; retain those historical accounting limitations.

Finish the relevant default/new-feature source and replay-identity checks, then
publish this source and build it on ms02. Before a conditional policy panel,
separately register a short qualification that verifies the actual restored key
projection and a complete small campaign's recorded decisions, snapshots and
replay, including real alternate retention. A qualification failure stops its
allocation; do not turn it into a parameter or root search. No new native work,
policy panel, fresh seed or frame budget is allocated by this source design.
PC01 is closed; all earlier failed policies remain closed. msr1 is unavailable.
