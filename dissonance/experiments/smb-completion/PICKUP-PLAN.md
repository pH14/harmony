<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# SMB completion pickup plan

## Objective and boundary

Finish the game while improving the game-neutral searcher. SMB supplies only
mechanically observed state, deterministic execution cost, terminal outcomes,
and controller vocabulary. Neither operators nor search policy may encode a
route, shortcut, desired transition, coordinate window, or action sequence.

The searcher must discover which retained states, state classes, and empirical
steps are useful from their measured novelty and descendant yield per unit
cost. An SMB-specific adapter may decode state; it may not prescribe what the
search should do with a particular state.

## Recovered frontier

- C119 is the current valid source: world/level/progress `(7, 0, 236)`, 3,837
  frames in the level at the frontier, 3,297 actions, archive SHA-256
  `d9038c97...`, stored on `msr1` under
  `/root/harmony-smb-goal/dissonance-v2/target/smb-completion/c119-conquest/`.
- C120 ended at execution 33,879 with exit status 143 and did not produce a
  final archive. Its stream reached progress 236 by execution 11,192, much
  earlier than its exact C119 control, but made no further progress.
- C120's whole-history empirical table contained roughly 1.69 million steps.
  Re-serializing and hashing that table before every draw reduced throughput
  from roughly 26 to 2.7 executions per second. This is a searcher defect, not
  evidence against empirical steps.
- The cached-generation fix is implemented in the unified worktree. Its
  C120-sized benchmark measures the draw-time checkpoint at approximately 31
  nanoseconds on the Mac.
- Campaign boundaries currently reduce the source archive to one selected
  lineage. Earlier links show that this can discard either the deepest or the
  cheapest useful family and force later campaigns to rediscover it.

## Searcher changes

### 1. Make learned artifacts cheap enough to use

Cache the content hash of an empirical-step generation and refresh it only
when buffered evidence becomes visible. A draw records the generation and
current stream position without re-serializing the table. Represent visible
tables as immutable, shared generations; replay retains only generations
referenced by in-flight records and verifies their cached hash without cloning
the complete history for every version.

This preserves the deterministic table and stream semantics while making draw
cost independent of accumulated history size.

### 2. Continue search state across budget boundaries

Replace single-input campaign handoff with a versioned, game-neutral search
checkpoint that contains the bounded active population, entry liveness and
cells, snapshots, scheduler histories, empirical-step generation, worker
random-stream positions, counters, target/configuration identities, and the
parent stream digest. Write it atomically only after draining in-flight work at
an admission boundary. Increasing a budget starts a linked stream segment from
that checkpoint rather than bootstrapping a new campaign.

An older archive is never described as an exact continuation: it lacks active
status, snapshots, scheduler history, and RNG state. Import it explicitly by
replaying candidate inputs to reconstruct snapshots, initializing neutral
scheduler priors, and constructing a bounded deterministic population from the
adapter's dominance relation and measured execution cost. Keep multiple
nondominated lineages and stable tie-breaking; do not choose a game-specific
distance or region. Re-root retained entries if pruning would leave dangling
lineage references.

### 3. Learn allocation online

Generalize the existing cost-normalized draw budgets from per-parent caps to a
hierarchical allocator over target-provided state classes and parents. A
deterministic credit/debt schedule guarantees exploration within one global
budget; it does not multiply a literal draw floor by an ever-growing
population. Extra credit is earned only by recent retained-descendant yield per
deterministic cost unit, with integer arithmetic, stream-ordered updates, and
stable serialized-identity tie-breaking.

The minimum allocator is a new generic module parameterized by opaque parent,
class, and transition identities. It maintains global, class, parent, and
bounded class-transition histories. An ongoing deterministic exploration quota
prevents a class or parent from satisfying its floor once and then starving.
The coordinator reserves credit before dispatch, completes it in admission
order, and cancels it for duplicate skips; in-flight workers therefore cannot
all select the same apparently untried parent, and a skip is not mislabeled as
a zero-cost failure.

Once this allocator clears its preregistered predictive and outcome gates,
make it the live policy. Keep pinned-window, waypoint, and hand-ranked resume
policies only for decoding and replaying historical streams.

### 4. Learn mutation evidence without a chosen region

Remove the region filter from the next empirical-step generation. Fold steps
from retained transitions using generic event labels such as novel retained
descendant, improved nondominated cost, and terminal result. Use recency plus
bounded all-history evidence so the vocabulary adapts when the domain changes
without an operator naming where the useful evidence should come from.

Future live policies also stop using waypoint-dependent suffix lengths. If
multiple mutation families remain useful, record their opaque identities and
let the same outcome model learn their allocation.

## Fast experiment loop

After exact checkpoints exist, use continuation budgets of 100, 500, 2,000,
10,000, and 50,000 executions. A stage is a prefix of one resumable run, not a
disposable campaign. Before that implementation lands, bounded C119-based arms
are separate experiments and must not be called continuations.

1. Canary at 100 executions: verify startup, exact header, sentinel handling,
   throughput, memory, checkpoint round-trip, and sidecar progress. Stop
   immediately on launch failure, two missed heartbeat intervals, or a large
   sustained throughput loss against the same-binary control.
2. Development stages at 500 and 2,000 executions: run paired
   control/challenger seeds on the Mac and `msr1` concurrently. Continue only
   arms showing better retained-descendant yield per cost, target-provided
   coverage, or nondominated cost, without losing already reached terminal
   outcomes.
3. Scale stage: continue survivors to 10,000, then 50,000 only while their
   curves or useful archive coverage are still moving.
4. Promotion gate: reproduce the improvement on held-out seeds and run one
   exact stream replay. Failed or flat ideas are recorded and abandoned at the
   earliest stage that distinguishes them.
5. Completion gate: only the winning lineage receives full campaign replay,
   power-on replay, artifact preservation, film rendering, and cross-machine
   replay.

Routine per-link replay audits are removed from the discovery loop. Recorded
streams and immutable inputs remain sufficient to perform the deferred audit
when an arm earns promotion.

The run wrapper must signal campaign completion before optional ladder and
census diagnostics, build each immutable binary once, use unique output
directories instead of deleting destinations, preserve partial streams as
incomplete evidence, and distinguish `CAMPAIGN_DONE` from `RUN_EXIT`. A waiter
timeout triggers a direct process and heartbeat check, not an inferred result.

## Immediate sequence

1. Land and verify the campaign-unification refactor without rewriting its
   uncommitted history.
2. Apply the empirical-table cached-generation fix and benchmark it against a
   C120-sized table.
3. Add resumable search checkpoints, initially preserving the current selector
   byte-for-byte within a continuous run.
4. Measure the learned-step policy against an unmodified-policy control from
   the same C118/C119 source conditions; C120 is incomplete evidence, not a
   result. Keep the allocator disabled in this pair.
5. Measure the generic hierarchical-yield allocator with learned steps
   disabled, then combine them only if both changes individually earn
   promotion.
6. Add and test multi-lineage legacy import before calling any C119-derived run
   a continuation.
7. Promote only measured searcher improvements, then continue the best search
   until the completion terminal is discovered.
