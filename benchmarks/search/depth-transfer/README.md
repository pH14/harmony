# Depth transfer after parallel research

This continues the [selection study](../continuation-yield/SUMMARY.md) after
the user's renewed allocation of msr1 and ms02. The [resumed goal](GOAL.md)
records decision gates and resource bounds. Earlier accounting remains frozen:
489,962,470 admitted search frames and 30,112,297 known auxiliary frames in the
continuation-yield tranche. New work has a separate ledger.

## Integrated ideas

Read [PR #285](https://github.com/pH14/harmony/pull/285) at
`ca85b3fa8e8e1b70cd98ccd85903d29dfdfe9409` and
[PR #288](https://github.com/pH14/harmony/pull/288) at
`ea757f23dd9998a7d82c3e444d99855dc509788c`, including their summaries, protocols,
feedback adjudication and relevant implementation. Their useful contribution is
a stricter causal standard and additional negative evidence.

| Finding | Integration / decision |
| --- | --- |
| #285 corrected terminal eligibility, but its complete boss panel was 0/10 versus 0/10 with mixed or worse secondary outcomes. | Our already versioned equivalent health-underflow predicate remains fixed in both arms. Mechanical correction is not promoted as a performance gain. |
| #285's `no_cell_cost` policy removes the same between-cell cost-rank term as our `no_cost` policy; ours also changes within-cell selection. | Record overlap instead of claiming an independent new idea. Ordinary Metroid's at-most-four-entry geometry makes its within-cell probability ranks inactive, but deterministic tie/order coupling and different experiments prevent pooling results or asserting identical streams. Test later-depth transfer explicitly. |
| #288's equal-frame probe found all discarded-exclusive maps already on source trajectories; no capability or boss gain. | Do not reopen behavioral retention from the prior 156-versus-42 exit count. Require equal physical work and a useful endpoint, with producing-snapshot and suffix-boundary verification. |
| Source-path unions are lower bounds on prior coverage, not the current active archive. | Membership can reject a novelty claim; absence cannot establish global novelty. Past visitation also does not prove current recoverability. |
| Both MM2 chains can extend while remaining at Wily 1; more attempts and duration variants failed to recover depth. | No unchanged chain or duration sweep. A transfer diagnostic must identify a more sensitive useful outcome before another allocation. |
| Area entry is distinct from encounter, damage and defeat. | Reuse our existing E01/E02 raw boss-memory probe, complete missing development evidence, and require a positive episode before adding campaign counters. |
| Resource retention with corrected terminal semantics was not tested. | Keep this interaction unmeasured; neither infer failure nor fund it solely from old local probes. |

The terminal predicates' relevant executable condition is the same decoded
`health >= 8000` under the explicit corrected policy. The related selector
comparison is at the conditional distribution level; the earlier Metroid
geometry argument does not imply adaptive improvement or pathwise identity.
Neither local reach nor sampled equal-work equality proves future equivalence.

## First registered step

[C01](c01-registration.json) freezes four ordinary controls on msr1, first
energy tank at 50M admitted frames, with exact build, endpoint and runner hashes.
At least two budgeted hits and complete measurement are required before a fresh
paired development panel. No candidate is tuned using favorable calibration
seeds. `continuation-yield/run_cells.py` executes the registration using the
previously qualified native milestone-stop build; no production change is
required for this step. Qualification failures remain in their original files.

The x86 build and short reused-fixture qualification run on ms02 concurrently.
Each future performance panel keeps candidate/control resource comparisons on
one host and one CPU set; architecture-specific binary/core identities remain
visible. Development and independent confirmation are scored separately.

## Completed qualification and diagnostics

[QX01](qx01-qualification.json) qualifies the native x86 build on ms02. The
default 2,000-job fixture passes full replay; one-slot and two-slot milestone
runs stop after the same first-morph event at job 148 / 22,586 admitted frames,
with identical retained artifacts and 23,883 total frames including drain.
The matching ARM semantic counts do not imply cross-architecture snapshot or
resource identity. The [checker](verify_qx01.py) consumes existing artifacts
without rerunning emulation. Its 669,822 known auxiliary frames include replay.

The QX01 launch accidentally recorded a placeholder registration-commit label.
The raw result is preserved. The [binding](qx01-registration-binding.json)
verifies its registration bytes against commit `55d3e16b`, which existed before
launch; this is a provenance correction, not a replacement experiment.

The previously registered E02 seed-5 diagnostic is now
[complete](e02-seed5-results.json). All three independent replays agree, with
383,796 physical frames including setup. Its selected Ridley-area route has
zero guarded loader or active-tag episodes. Together with the earlier seed-3
result, this completes the two-route diagnostic; it does not establish absence
across either producing campaign. An initial launch failed before emulation
because `/usr/bin/time` was unavailable; the
[resumption record](e02-seed5-resumption.json) preserves that zero-frame failure.

The [auxiliary ledger](auxiliary-ledger-after-qx01-e02.json) records 1,053,618
known frames for these completed operations, separately from ongoing search.

[F01](f01-observer-registration.json) registers a bounded positive control for
the existing raw observer, using Lord Tom's public
[Metroid movie](https://tasvideos.org/1320M) on FCEUX. This is an independent
reference emulator; the movie's routes and states never enter fresh search.
A complete replay must reproduce a guarded defeat transition before a second
process is allowed. Loader/active-slot agreement alone is insufficient to claim
damage. A mismatch is retained as a decoder falsifier, not fixed by extending
the search budget. The [Lua observer](observe_fm2.lua) only reads memory.

### Positive encounter control and a damage-filter counterexample

F01 initially failed its first frame-clock assertion. The frontend continued
until stopped 29 seconds after launch: 29.458 CPU seconds, with unmeasured frame
advancement. The failure earns no positive evidence. A separately bounded
[20-call diagnostic](observer-clock.csv) observed movie clocks `0, 0, 1, …, 19`.
[F01r](f01r-observer-registration.json) adds that single startup synchronization
yield and makes checked failures stop playback and exit. Both independently
started corrected replays completed all 120,540 movie frames and produced
[identical summaries and raw traces](f01r-results.json). This qualifies an
encounter-entry positive control, with 319 loader/active-tag agreement frames.

The [filtered trace analysis](f01r-lifecycle-analysis.json) then exposed a
measurement problem: every observed HP difference crossed a filter gap, and the
loader had cleared before damage. The pinned disassembly explains both effects:
door-transition logic clears `KrdRdlyPresent`; the hit path changes enemy status
and overwrites the special byte before decrementing HP. Neither field is a
persistent fight identity.

[F02](f02-lifecycle-registration.json) retains every boss-area frame using the
same memory reads, movie and emulator. Applying the old filter reproduces F01r's
exact trace, and two new processes produce identical complete traces. The
[lifecycle analysis](f02-lifecycle-analysis.json) finds:

| Reference fight | Continuous frames, entry through defeat | HP changes | Total HP decrease | Changes with current loader / special tag |
| --- | --- | --- | --- | --- |
| Ridley | 581 | 62 | 140 → 0 | 0 / 0 |
| Kraid | 400 | 36 | 96 → 0 | 0 / 0 |

Both lifetimes keep the same area, slot and data index, with no HP increase or
premature inactive slot. HP reaches zero 24 frames before the defeat flag and
slot retirement. All 98 decreases occur in hit/death states with the special
tag overwritten. Mandatory current-loader or current-tag guards would miss
every one. The [complete compressed trace](f02-boss-area-frames.csv.gz) preserves
the raw evidence; it contains observations, not controller inputs.

This supports episode-entry anchoring followed by continuous enemy-lifetime
observation. It does not qualify identity across snapshot restores, inactive
slot reuse, area/type changes or HP increases. Those boundaries must invalidate
or close an episode before campaign counters are added under
[issue #281](https://github.com/pH14/harmony/issues/281). One public movie with
two fights, replayed independently, is observer evidence; no fresh search
success or QuickNES cross-emulator replay is implied.

F01r and F02 each spend 241,080 known movie frames; the clock diagnostic adds 19.
Together with QX01/E02, that is 1,535,797 known auxiliary frames before C01's
witness work. Failed-F01 advancement and frontend setup remain unmeasured.

## Calibration result and paired allocation

[C01 passed](c01-analysis.json): three of four ordinary controls attained an
energy tank within the fixed 50M-frame horizon. Exact admitted arrival costs
were 40,909,271, 33,896,740 and 48,845,104; the fourth was censored at 50M. All
four measurements passed. Actual admitted work including drain is 173,654,997
frames, with 711,296 known twice-replayed witness frames. This qualifies the
endpoint for allocation; it does not compare selectors or guarantee power.

[D01](d01-registration.json) froze four fresh paired seeds on ms02 CPUs 0–3,
50M frames per arm, with balanced sequential arm order and no concurrent
performance job on that host. Three strict wins, at least 15% lower mean
restricted cost, and CPU/elapsed ratios at most 1.25 are required. The complete
eight-cell wall bound is reserved before launch. Retention, terminal policy,
actions and endpoint remain unchanged. A passing result earns independent
confirmation on msr1; an impossible win count stops further pair dispatch.

[D01 failed its allocation gate](d01-analysis.json) after two complete pairs:

| Paired seed | Control restricted cost | Candidate restricted cost | Result |
| --- | --- | --- | --- |
| 1695811351 | 50M, censored | 50M, censored | Tie |
| 1437316129 | 48,303,571, hit | 50M, censored | Loss |

Three wins became impossible, so the runner stopped the remaining two pairs.
Actual admitted work was 198,308,020 frames, plus 546,224 known witness frames.
The four-pair cost/resource result is unmeasured; independent confirmation was
not earned. This does not negate the separately replicated first-missile
precursor result or establish population equivalence. It stops this later-depth
allocation under the unchanged endpoint and horizon.

## Native diagnostic correction

The standalone probe now accepts the optional `boss-area` trace mode. Its
[native qualification](nq01-results.json) passes on the already observed E02
seed-5 route: default summary/trace bytes match the old binary exactly, and the
dense mode adds 29,911 area frames while preserving all-frame hashes, emulator
endpoints and all three replay comparisons. It spent 767,592 physical frames
including setup. The hit-state regression and all-feature probe Clippy pass.
The new source/build stays separate from D01's frozen performance binary.
This qualification adds 767,592 known auxiliary frames to the ledger.

[B01 passed](b01-analysis.json) after D01 finished. C01's deliberately reused
control-1 on x86, at a fixed 35M horizon, reproduced the known 33,896,740-frame
event and 33,897,593 total admitted frames exactly. Main and milestone action
tapes and replayed semantic observations match across hosts. Its 34,099,797
known frames, including witness replay, are qualification work and add no fresh
performance sample. This is a longer platform check, not universal cross-host
identity or a cross-host resource comparison.

The [completed ledger](ledger.json) totals **371,963,017 admitted search frames**
and **37,660,706 known auxiliary frames** for this renewed block. Failed-F01
advancement, setup and unadmitted-work gaps remain explicit. The next
[goal step](GOAL.md) is to qualify encounter/lifetime observation across restore
boundaries and use bounded existing-tape evidence before another search family
or larger allocation. The original boss/Wily breakthrough remains unachieved.
All work remains in PR #287.


## Saved status and a snapshot-local observer

[F03](f03-analysis.json) adds the saved status/attribute byte at `$040C + slot`.
Both full reference replays agree; projecting away the added columns reproduces
F02 exactly. Normal enemy states must use current attributes because the saved
byte can be stale; hit/death states recover the prior boss attributes from the
saved byte. This removes the need to inherit an encounter-entry anchor across
restores. The [contract](boss-observation-contract.md) states the source argument,
interval definition, reset properties and unresolved same-key reload limit.

The Python reference retains all 98 HP decreases and checks every stored cut,
plus planted restore/gap/reuse counterexamples. Rust checks the same 981 positive
fight frames and boundary cases. These are cheap offline checks, not new search
results or a positive native-emulator restore qualification. The new optional
standalone `boss-context` mode reads both RAM regions at one frame boundary;
`boss-context-restores` additionally performs real self-restores every 4,096
route frames. The default and `boss-area` contracts stay unchanged. Native
qualification must finish before examining another frozen development tape set.

The [additive F03 ledger](ledger-after-f03.json) records **37,901,786 known
auxiliary frames**; admitted search stays **371,963,017**. Historical files and
unknown physical-work gaps remain preserved.
