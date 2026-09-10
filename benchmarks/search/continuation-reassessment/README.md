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

FW02 completes all five inputs: each held-command replay agrees with two
independent one-frame replays. Together with the already completed FW01 input,
all six original witnesses retain Bombs under corrected terminal semantics.
All six end in mode9. FW02 takes209.707 service seconds,208.687 CPU seconds and
16,384,000 peak service bytes; its final journal supplies exact resource fields
after the successful transient unit unloads. Its950,274 new frames plus FW01's
234,966 total1,185,240, without replaying or double-charging the first input.
See [the closed ledger](ledger-after-fw02.json) and
[offline verifier](verify_fw02.py). Resumed cumulative work is1,273,426,135
admitted search frames and62,090,185 known auxiliary frames. Prior unknown work
and expired-tranche totals remain separate. This is compatibility, not efficacy.

## Current control evidence changes the scale of a fresh test

The already completed corrected-terminal C01 control supplies a second caution.
[audit_c01.py](audit_c01.py) verifies all19,961 saved progress records against
the exact published summary and preserves its original incomplete horizon.
Its reused seed5 reaches Bombs at113,325,933–113,337,939 admitted frames, versus
263,199,528–263,211,554 in the historical control. First energy-tank cost is
unchanged at19,073,266–19,084,754. C01 later stops at its original35-minute wall
cap with240,942,610 frames; no extension or replacement was run. All named
attainments and censoring remain in [c01-arrivals.json](c01-arrivals.json).

C01 uses terminal v3 and key v10; historical012 uses terminal v2 and key v8.
These records do not isolate the effect of either change. One reused,
outcome-selected control is not a fresh calibration pass. The large late
baseline difference makes it especially unsafe to carry the old43% candidate
gain forward. The six compatible tapes establish a basis to test the unchanged
continuation policy, not the size or sign of its current effect. No gain or
failure at an earlier endpoint substitutes for a frozen deeper comparison.

The current work counter already includes origin reconstruction performed by
each admitted job: the worker samples `frames_clocked` before `execute_job`,
and the Metroid job restores its snapshot, applies the replay prefix, then
executes its suffix. QuickNES's lifetime `now()` does not rewind on restore.
Thus per-job replayed prefixes cannot be treated as free in a new comparison.
Worker setup and final witness verification remain additional costs; preserve
any unmeasured work at failed/incomplete boundaries. This source finding does
not retroactively fill historical accounting gaps or equate CPU costs.

The next bounded qualification should use the existing corrected-terminal
milestone-stop binaries, requiring actual continuation dispatch, full campaign
replay, same-host result-buffer identity and cross-host event identity. Failure
stops allocation. A pass may earn a separately frozen fresh Bombs panel with
matched admitted work/memory, same-host paired CPU placement, resource limits,
and the existing three-win/15% restricted-cost gate. Early milestones remain
secondary; no new queue order, bank, suffix or retention setting is bundled in.
Independent confirmation, MM2 transfer and untouched boss/Wily validation remain
required. Recompute the completed evidence without emulation with:

```sh
python3 -m unittest discover -s benchmarks/search/continuation-reassessment -p 'test_*.py' -v
```

[CQ01](cq01-registration.json) now freezes that qualification: reused seed
2026090902, two ARM buffer variants and one x86 cell, each capped at2,000 jobs,
500k admitted frames and180 search seconds. The existing binaries, assets,
8GiB archive, ordinary cutoff3 selector and one-to-six suffix stay fixed;
only the existing `alphabet_continuation_v1` mixture is enabled. Full replay and
actual fourth-reservation continuation dispatch are required. Each process has
12GiB service memory,1GiB output and330 seconds including finishing; the ARM
two-cell service has720 seconds and x86 has360. The separate known auxiliary
ceiling is8M. No fresh search, retry or automatic larger fixture is registered.

CQ01 passes: all three runs execute190 continuation jobs among2,000 admitted
jobs, at275,130 frames each. Every continuation uses its fourth reservation and
recorded tail. Full replay succeeds, both ARM buffer variants have identical
stream/campaign/checkpoint artifacts, and ARM/x86 post-header events match.
The cells take18.28/17.28/8.04 seconds and charge1,670,658 known auxiliary frames,
including inferred full campaign replay and twice-replayed held witnesses.
Both services deactivate successfully. The exact source, native output, service
journals and [closed ledger](ledger-after-cq01.json) remain available; cumulative
resumed known auxiliary cost is63,760,843, with search unchanged at1,273,426,135.
Initial read-only preflight used an older runner helper from the source snapshot;
supplying the exact already-registered helper files fixed that path mistake
before any emulator execution, without changing a registration or native input.

The next fresh comparison should target Bombs with the unchanged candidate.
Current timing argues for ms02's eight faster cores (0–7), split into two
nonoverlapping four-core groups with sequential, balanced arms within each pair.
ED01's50M cells take about357 seconds on ms02 versus862 on msr1; those timings
are feasibility evidence, not a throughput guarantee at greater depth. The
current corrected control's113.33M Bombs arrival and the historical candidate's
88.8–263.3M range make a200M fixed horizon a reasonable bounded screen to
consider; both-censored pairs must remain ties. A larger horizon is not earned
by failure. Freeze all four new pairs, arm order, full wall reservation, work,
memory and output bounds before dispatch. Run only the first two pairs initially
and stop if the fixed three-win gate becomes impossible. CQ01 does not itself
register or launch that performance panel.

After CQ01 closed, the user reassigned msr1 to another task. Its research jobs
were already terminal and all evidence had been copied locally. Subsequent
experimentation, including confirmation, must use ms02; msr1 is unavailable.

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
