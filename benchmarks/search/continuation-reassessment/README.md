# Reassess continuation at the depth where it could help

The offline audit finds a concrete surrogate reversal in the existing 012
Metroid experiment: alphabet continuation takes about 43% more admitted work
to acquire the first energy tank, but about 43% less to acquire Bombs. This
changes the next research question. It does not reopen the failed selection,
retention, action-correlation, retry or cutoff panels, or establish a boss gain.

## Recover actual work from the historical records

[audit.py](audit.py) reads all 180,012 saved progress records from the six long
012 cells. Raw summaries match the committed `results/remine-012.json` after
its documented witness-hash projection and added selector-accounting field.
Each raw result matches its summary. The audit verifies monotonic admitted
execution/frame counts, persistent first-event identities, terminal totals and
the presence of each attained milestone's original replay evidence.

For an event first admitted at execution E, the last checkpoint before E gives
the strict lower work bound and the first at or after E gives the upper bound.
An exact checkpoint at E collapses that interval. Route-frame timestamps are
never substituted for admitted search cost. All six runs attain the four
milestones below, so their means contain no success-only exclusion.

| Milestone | Continuation/control mean first-arrival frame-cost interval | Strict paired wins |
| --- | ---: | ---: |
| First missile capacity | 1.18858–1.18980 | 2/3 |
| First energy tank | 1.42951–1.43027 | 1/3 |
| Bombs | 0.56772–0.56781 | 3/3 |
| Kraid's area | 0.59100–0.59107 | 3/3 |

| Seed | Control first Bombs, admitted frames | Continuation first Bombs, admitted frames |
| --- | ---: | ---: |
| 3 | 363,700,295–363,712,621 | 173,632,256–173,646,518 |
| 4 | 299,041,623–299,056,399 | 263,270,557–263,285,051 |
| 5 | 263,199,528–263,211,554 | 88,803,718–88,818,147 |

Seeds3/5 lose at energy-tank acquisition and win at Bombs. The intervals are
checkpoint uncertainty, not statistical confidence. All reported milestones,
including nonattainment, remain in [analysis.json](analysis.json); censored
endpoints receive no success-only mean. Long Beam and Ridley-area regressions
remain in the original evidence. Neither arm defeats a boss.

These are reused, retrospectively inspected development seeds, originally
stopped at three million jobs with unequal final frame totals. They are not a
new paired screen or independent confirmation. The six cells ran concurrently
on different CPU sets; this audit makes no matched CPU/wall comparison. They
also used legacy terminal v2 and key v8. Current corrected-terminal v3 efficacy
is unmeasured. The first-event records on different branches are not combined
into one route or interpreted as a causal mediation experiment.

Lossless copies of all eighteen source files are in `evidence/`, with raw
hashes, sizes and the ms02 source root in `evidence/manifest.json`. This audit
uses zero emulator frames; it does not recharge the historical runs to a new
ledger. Recompute it and its planted-evidence check with:

```sh
python3 -m unittest discover -s benchmarks/search/continuation-reassessment -p test_audit.py -v
```

## What this changes about the scientific gate

An earlier milestone is a measurement opportunity, not a validated surrogate
for later success. Even if an early event were necessary on every successful
route, T_early ≤ T_late within each policy would not order two policies' late
costs. Costs (1,100) versus (2,3) are a finite counterexample. The historical
seed-level reversal demonstrates the practical issue here without assuming
that an energy tank is a prerequisite for Bombs.

A future continuation study should choose a deeper useful endpoint before fresh
execution, and retain early milestones as secondary costs. It must not demand
that every earlier milestone improve. This changes future allocation reasoning;
it does not rescore any closed preregistered panel. Bombs remains a precursor,
and area entry remains distinct from an encounter or defeat.

The existing `alphabet_continuation_v1` is the candidate to reassess, unchanged.
It transfers observed exits after same-slot preference improvement, with bounded
storage, a quarter reservation share and separate exploration accounting. Its
plausible benefit occurs after useful transitions have accumulated. That is a
hypothesis, not an attribution established by these aggregate observations.
Its observed early regressions and lost later discoveries are adverse evidence.

The next cheap question is whether the six exact first-Bombs witness inputs
remain valid under corrected terminal semantics. Use the already qualified
probe and bind all six inputs by the historical witness hashes; no replacement
tapes, added observer, new route or fresh-search input is needed. A separately
registered short qualification must bound all three replays and setup work,
memory, output and wall time. A failure closes this historical basis for
escalation. A pass establishes witness compatibility only; it would still
require a separately frozen fresh development panel, independent confirmation
and MM2 transfer before the original untouched boss/Wily validation.

[FW01](fw01-registration.json) now freezes that compatibility check on msr1's
unchanged qualified binary: six sequential inputs, one CPU, 4GiB service memory,
180 seconds per case and 20 minutes for the block. Expected three-pass work is
1,185,240 frames including setup, below a separate 1.3M ceiling. The dispatch
deadline is 2026-09-10T10:11:58Z. Four offline tests use a prior qualified native
result and reject missing replay, terminal underflow and missing Bombs evidence.
This registration precedes all FW01 execution.

FW01 stopped after its first input: all three native passes completed and agreed
on Bombs, health85 and endingfalse, but the checker required final mode3 and
rejected mode9. That guard was stronger than the adapter contract. In
`metroid/target.rs`, `MetroidTerminalPolicy::is_dead` checks zero/underflow health;
`in_play` is a separate predicate. `Evaluation::is_terminal` in `campaign.rs`
checks death, victory and emulator errors, without a gameplay-mode requirement.
Milestones can be observed inside a held command whose complete input ends in
another mode. No interpretation of mode9's animation is needed for this finding.

The original [FW01 result](fw01-output/results.json), runner and registration
remain failed and unchanged. Its [closed ledger](ledger-after-fw01.json) charges
234,966 known frames; all three passes completed, with no unknown partial work.
The remaining five inputs were not run. This is a measurement-contract error,
not a failed policy comparison or a reason to rewrite a performance gate.

[FW02](fw02-registration.json) separately freezes the corrected predicate and
the remaining five inputs on the same binary. It rechecks the existing first
input's artifacts without rerunning or recharging them. Six new offline tests
use that actual mode9 result, preserve the original failure, and still reject
terminal states, missing Bombs, incomplete replay and a wrong input identity.
The new block expects 950,274 additional frames including setup, capped at1M,
with the same one-CPU,4GiB,180-second case and20-minute block limits. Stop at its
first failure. No fresh-search panel is allocated by this correction.

## Ordering findings stay separate from this candidate

Two new source fixtures expose existing limitations without changing policies:

- `legacy_splice_availability_changes_under_location_relabeling` keeps one
  learned route, the selected parent, insertion order and semantic progress
  relation fixed. Exchanging the two opaque location names changes a legacy
  ordinary splice from a one-action tail to unavailable. The full-key descendant
  cache is independent of semantic parent selection, as tracked in issue #270.
- `legacy_partial_batch_priority_depends_on_destination_labels` enumerates all
  six renamings of three observed exits. The first dispatched action changes;
  the complete drained set does not. Improvement batches are newest first, but
  within a batch `BTreeMap` traversal followed by `pop_back` tries larger
  destination names first. Finite dispatch opportunity can expose that bias.

Both paths are disabled under ED01's `alphabet_only`. An ordering-only change
cannot improve that baseline. The new fixtures demonstrate conditional source
behavior, not prevalence or useful lost transitions in a native campaign. Do
not combine an ordering fix with the unchanged continuation candidate: doing
so would abandon the mechanism supported by the historical comparison.
