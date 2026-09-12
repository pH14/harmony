# RR02: observed loss of lower-HP states with equal player resources

The reconstructed ms02 campaign rejected 22 candidates whose classified boss HP
was lower than the retained competitor's, with exact equality in all six
registered player-resource fields. These are 22 distinct candidate snapshots and
inputs competing against 10 retained snapshots within one fixed pilot; they are
not independent fresh-search trials.

| Frozen query category | Count |
| --- | ---: |
| Complete local competitions | 3,770 |
| Two live, comparable classified boss states | 1,717 |
| Unclassified, therefore unavailable comparisons | 2,053 |
| Rejected candidate with lower boss HP | 22 |
| Those rejections with equal measured player resources | 22 |
| Displaced incumbent with lower boss HP | 0 |

The [frozen earliest pair](rr02-earliest-pair.json) is competition 692, execution 423,
against incumbent 86. The rejected candidate has boss HP 137; the retained state
has HP 140. Both have health 79 (7.9 energy), equipment 17, one energy tank,
missile capacity 20, zero missiles and zero boss-defeat flags. Samus's positions
are (138,129) and (140,129), poses 0 and 1, and motion metadata 13 and 10. The observed
boss statuses are 6 and 2. Equal player resources do not make the other state
differences disappear or isolate a causal effect of HP.

This establishes a finite local representation loss in the reconstructed run.
It does not show that the rejected state has more useful futures, that an HP
preference improves retention, or that a fresh seed defeats a boss. The earliest
pair remains fixed for the next decision; do not select another pair based on
continuation outcomes or promote an arbitrary HP bucket.

## Qualification, scope and cost

The normal action-prefix reconstruction matched every saved root field and
emulator byte outside the independently verified core hash. Qualification passed
four original jobs/865 frames in 2.804 seconds. Full inspection independently
regenerated the same runtime root/origin, replayed all 2,548 jobs/250,267 frames
with original result digests and ordered admission decisions, and matched all 588
final checkpoint IDs and complete snapshots under the same comparison relation.
It then finished and read all 3,770 capture rows and paused contexts in 8.612 seconds.
The independent Python checks reproduce root correspondence, exact selected job
bytes, native classifications, complete query results and the evidence hashes.

These checks support compatibility on the known artifacts. They cannot prove
identity of discarded ARM snapshots that were never recorded. All local-loss
claims here refer to the reconstructed ms02 campaign. RR01's failed raw import
and its original registration remain unchanged.

The controller ran 11.419 seconds. The service deactivated successfully, using
11.230771 CPU seconds and 278,851,584 peak memory bytes (about 266 MiB). Direct
counters and completed replay reports charge 488,740 known auxiliary frames:
1,858 inspector setup, 235,750 reconstruction and 251,132 campaign replay. The
generic replayer's constructor counters remain unexposed; they are not zero.
No fresh-search frames were allocated or charged. See the
[closed ledger](ledger-after-rr02.json) for cumulative totals and historical gaps.

All 25 original output/service artifacts are preserved in a deterministic gzip
bundle of 10,367,376 bytes, with raw and compressed sizes/hashes in the
[manifest](rr02-evidence-manifest.json). The capture retains the complete paired
snapshots and checkpoint-local inputs. Reproduce the frozen analysis and closure
without an emulator:

```sh
python3 benchmarks/search/continuation-reassessment/verify_rr02.py
```

## Next decision

Design a bounded paired continuation counterexample from the already selected
complete states, using the same predeclared continuation inputs and physical
horizon for both. Distinguish pre-existing HP from new progress and actual defeat;
preserve nonattainment, terminal outcomes and unavailable encounter identity.
The current states differ in more than HP, so do not substitute a counterfactual
HP edit or claim scalar-HP dominance. First export and verify the exact selected
record through the existing capture reader, without emulator work. Any subsequent
continuation execution needs a separate prospective registration and stop gate.

No retention policy, fresh performance panel, transfer or untouched validation is
earned by this query alone. The original matched boss/Wily goal remains active
and unachieved; msr1 remains unavailable.
