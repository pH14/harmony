# PQ01: preserved progress and delayed returns

The progress arm's saved states reach **boss HP94**, versus **HP133** in ordinary,
with player health79 and zero missiles in both best states. This is meaningful
conditional proxy progress in the existing recorded run. Neither arm defeats the
boss, PG02 remains invalid, and its capacity comparator is unrun. No efficacy
pass, general improvement rate or fresh-search result follows.

## What the saved states establish

The query reads every cached snapshot from both original PG02 checkpoints. Raw
contexts match their qualified origin, and each restore/read preserves the full
snapshot and frame clock. Both calls pass: **3,445 cached states plus two origin
controls = 3,447 checked restores**. There are no gameplay actions, new seeds,
continuations, campaign replays or selected new roots.

All classified states below use the same exact qualified boss/capability scope
as the original HP140 encounter. Unclassified cached states are reported separately.

| Cached-state measure | Ordinary | Progress |
| --- | ---: | ---: |
| Total cached states | 1,752 | 1,693 |
| Same-scope states with available boss HP | 337 | 558 |
| States below root HP140 | 150 | 397 |
| Those states subsequently used for an executed job | 144 | 333 |
| Executed parent choices from those states | 806 | 1,403 |
| Lower-HP states at original health79/missiles0 | 99 | 338 |
| Best snapshot-local boss HP | 133 | 94 |

The frozen analyzer's parent_draws field counts executed job records. Each source
stream also contains two skipped proposals, which are excluded from those counts.
This distinction matters: these are executed extensions, not all selector calls.
Cached reconstruction anchors may be inactive; the query does not label these
rows as the final active population. Conditioning on the final snapshot set
excludes discarded states and cannot estimate their lost opportunities.

A concrete recorded return links progress ID2014 to ID2572. ID2014 has HP95 and
health79, is created at job5436, and receives its first executed selection at
job7362. That job runs 123 frames and retains ID2572 at HP94 and health79. ID2572
is first selected at job9979. Thus the observed gaps are 1,926 and 2,617 jobs.
They are descriptive delays; neither continuous eligibility nor the probability
of improving under another sampling rule is measured.

These data reject a blanket explanation that lower-HP states are absent or never
revisited. They motivate a more precise next question about return allocation.
The bounded same-slot proposal and its conditional probability identity are in
[scoped-return-design.md](scoped-return-design.md). Source admissibility and an
executable falsifier must be checked before implementing or allocating that policy.
The hypothesis earns no automatic longer horizon and cannot repair PG02.

## Cost, provenance and closure

The service runs on ms02 CPU8 for **0.678207 seconds**, with native child wall times
0.302910/0.303136 seconds. It uses exactly **1,858 physical frames**: two ordinary
929-frame constructor initializations, then zero frames during paused reads.
Measured service CPU is 0.644752 seconds; peak memory is 146,354,176 bytes under
the 1 GiB cap, with zero swap. The service is explicitly stopped and inactive,
and the unused allocation is released. msr1 is never accessed.

The inspector's only source change raises its bounded input allowance to 64 MiB
and 2,048 unique entries, accommodating these 38.5/37.2 MB saved checkpoints.
No search, selection, retention or projection code changes. The typed structural
test and strict feature-caller Clippy pass. Local inventories use zero emulator
frames. Three offline tests check the actual earlier inspector schema, changed
clock/root/inventory/completion, and unavailable HP versus defeat flags.

Native source is e00b9cc8413ca7f6737d0b34f1636d477bc16287; binary SHA256 is
0deb440b2d605e8d83622a3fee7d9089a5abbedf90b73f1dd472cf9fb6ec838e.
The separate registration is published in 77a91471 before native initialization.
Normal registration pre-push checks pass 1,184 tests with 23 skipped.

The offline verifier checks all 13 native artifacts, frozen protocol/build and
analysis dependencies, exact inventory correspondence, source-stream analysis,
physical costs, resource receipts and stopped service state without emulation.
Full per-state analysis is compressed in pq01-analysis.json.gz; pq01-summary.json
contains the compact distributions. The readable tables summarize cached states
only, with the limitations above.

The ledger adds zero admitted / 1,858 auxiliary frames. Resumed totals are
2,985,486,535 admitted / 74,268,821 known auxiliary. Historical incomplete costs
remain as recorded. Fresh development, independent confirmation, MM2 evaluation
and untouched validation remain required by the active, unachieved goal.
