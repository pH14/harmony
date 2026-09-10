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

## CD01: fresh first-Bombs development

[CD01's decision](cd01-decision.md) and [registration](cd01-registration.json)
freeze four fresh pairs comparing `alphabet_only` with unchanged
`alphabet_continuation_v1`, first Bombs at200M admitted frames. Only the mixture
differs within each pair. Source, key/terminal semantics, selector, suffix,
memory and power-on genesis match. Seed audits find no matches in212 local
tracked records and1,892 saved ms02 records; reassigned msr1 is not accessed.

Run pairs0/1 first on ms02 cores0–3/4–7 with sequential arms. Only an attainable
gate earns pairs2/3; each CPU group sees both arm orders over the complete panel.
A pass requires three strict wins,15% lower mean restricted cost and CPU/wall
ratios at most1.25. Censored ties, incomplete horizons and resource failures are
preserved; none earns a longer horizon or replacement seed. Earlier milestones
are secondary and cannot rescue the registered endpoint.

Each cell has3M jobs,4,096 actions,8GiB archive,12GiB service memory,4GiB output,
45 search minutes and180 finishing seconds within a2,910-second process limit.
Nominal search is capped at1.6B plus actual bounded drain; known auxiliary work
has a separate32M allowance. Continuations can exceed six actions, so the
conservative drain bound includes a whole4,096-action replay-plus-suffix job
for each of eight outstanding reservations:4,194,304 frames per cell. This
grants no score beyond200M. The fixed experiment deadline is2026-09-10T14:32:28Z;
the following hour is reserved for synthesis. No earlier tranche is extended.
This registration precedes CD01 native execution.

CD01 is complete. The unchanged [frozen scorer](score_cd01.py) records four
strict wins and a [development pass](cd01-analysis.json), earning independent
confirmation of the unchanged candidate and endpoint.

| Fresh seed | Control first Bombs | Continuation first Bombs | Result |
| --- | ---: | ---: | --- |
| 1040692292 | Not attained by 200M | 106,972,762 | Strict win |
| 2883914186 | 140,516,627 | 98,489,894 | Strict win |
| 4193773728 | Not attained by 200M | 69,721,067 | Strict win |
| 2971383614 | Not attained by 200M | 91,627,528 | Strict win |

Continuation attains Bombs on 4/4 seeds versus 1/4 for control. Mean restricted
cost is 91,702,812.75 versus 185,129,156.75 frames: ratio 0.495345, or 50.5%
less admitted search work. Event-stopped CPU and wall ratios are 0.491853 and
0.519710. The censored controls count at the fixed 200M horizon; their eventual
arrival times are unknown. These four development pairs are an allocation
result, not a population effect, independent confirmation or a boss/Wily result.
All five Bombs witnesses replay alive. All eight runs match their registered
identities and CPU placement, stay within the resource bounds, and finish their
registered endpoint or frame horizon. Complete native archives, member hashes
and service journals are in
[cd01-output](cd01-output); [the offline verifier](verify_cd01_native.py) checks
their integrity and actual resource measurements without running an emulator.

The [closed ledger](ledger-after-cd01.json) charges 1,107,336,590 admitted frames,
including 8,712 stop-drain frames, and 2,534,350 known auxiliary frames. Resumed
totals through CD01 are 2,380,762,725 admitted search and 66,295,193 known auxiliary
frames. Setup, unadmitted work and any out-of-job reconstruction remain unknown.
The earlier [first-wave ledger](cd01-first-wave-ledger.json) is an included
historical subtotal; it must not be charged again. All four services terminated
successfully, with peaks below 3.2GB against their separate 12GiB caps.

The [recorded gate](cd01-second-wave-gate.json) and zero-emulation
[preflight](cd01-second-wave-preflight.json) permitted only the frozen remaining
pairs after two strict first-wave wins. Both started on ms02 at 11:23:33 UTC on
September 10, with opposite arm
orders from the first wave on their respective CPU groups. The
[dispatch record](cd01-second-wave-dispatch.jsonl) binds their service identities
and actual limits. Their full 5,900-second service bounds fit before the same
deadline, and both finished by 12:00:14 UTC. No registration, candidate, horizon,
seed or arm order changed. The source ordering limitations remain separate.

The next allocation is independent confirmation on fresh seeds at the unchanged
200M first-Bombs endpoint, source, selector and continuation policy. The current
effect size cannot select its seeds, shorten its horizon or substitute earlier
milestones. Keep the same four-pair, three-win/15% and resource rules, with
bounded waves and futility. A pass then earns a bounded same-mechanism MM2
transfer decision. Untouched boss/Wily validation remains unearned, and the
original goal remains active and unachieved.

Recompute the completed panel without emulation:

```sh
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cd01-output --pair 0 --out /tmp/cd01-pair-0-verification.json
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cd01-output --pair 1 --out /tmp/cd01-pair-1-verification.json
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cd01-output --pair 2 --out /tmp/cd01-pair-2-verification.json
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cd01-output --pair 3 --out /tmp/cd01-pair-3-verification.json
python3 benchmarks/search/continuation-reassessment/score_cd01.py --registration benchmarks/search/continuation-reassessment/cd01-registration.json --evidence benchmarks/search/continuation-reassessment/cd01-output --out /tmp/cd01-analysis.json
```

## CC01: independent confirmation failed

The [confirmation decision](cc01-decision.md) and
[registration](cc01-registration.json) freeze fresh seeds 833917373, 4204540845,
586667767 and 2160473381. The candidate, control, first-Bombs endpoint, 200M
horizon, source, assets, selector, suffix and resource limits are unchanged.
CC01 alone determines its gate; development outcomes are not pooled into it.
The [thin adapter](score_cc01.py) uses the same frozen paired scorer and endpoint
helper, with a separate confirmation decision label.

The local saved-record audit finds no seed match in 224 records; the ms02 audit
finds none in 4,830. Its initial decode failures are preserved and independently
identified as two AppleDouble metadata sidecars. Their actual JSON records
parse and contain none of these seeds. A transient local storage failure
prevented two read-only preparation commands from starting; clearing only this
task's rebuildable incremental compiler caches restored headroom. No emulator
work or seed change occurred during these preparation checks.

CD01 is complete and its unused allocation is released after synthesis and
publication. CC01 receives its own maximum experimental window, ending at
2026-09-10T16:27:03Z, followed by an hour reserved for synthesis. This does not
add development cells or alter any earlier registration, outcome or failed gate.
Start pairs0/1 only after publication and a zero-emulation preflight verifies
that both worst-case 5,900-second waves fit. Each started pair remains a bounded
sequential two-arm unit on one four-core group. A completed favorable wave may
earn only the already-frozen remaining pairs, subject to the same deadline.

Require three strict wins, 15% lower mean restricted cost, and CPU/wall ratios
at most 1.25. Preserve censored ties and incomplete measurements. A failure
closes this confirmation line; a pass earns a separately bounded same-mechanism
MM2 transfer decision.

Both first-wave pairs started on ms02 at 13:02:59 UTC on September 10 after
the [preflight](cc01-preflight.json) passed. The [dispatch record](cc01-dispatch.jsonl)
preserves the exact service handles and limits. Both services finished by
13:43:49 UTC, with all four cells complete. The [frozen score](cc01-analysis.json)
stops at zero strict wins: three wins in four pairs are now impossible.

| Fresh seed | Control first Bombs | Continuation first Bombs | Outcome |
| --- | ---: | ---: | --- |
| 833917373 | 139,784,604 | 182,858,882 | loss |
| 4204540845 | 129,156,979 | 149,917,731 | loss |

All four Bombs witnesses replay alive, and both independent native audits pass.
The complete [native evidence](cc01-output) includes raw archives, results,
service journals and verifier reports. The [closure audit](cc01-closed-service-audit.json)
confirms both services are terminal and pairs2/3 have neither outputs nor service
history. They remain unrun; no second wave or MM2 transfer is earned.

On the two completed pairs, continuation uses 23.7% more restricted admitted
work; its event-stopped CPU and wall ratios are 1.239 and 1.294. These describe
the completed wave only. The registered decision fails on win count, and no
full four-pair effect or population conclusion is available. The earlier four
development wins remain valid observations, but they did not replicate here.
Do not pool development with confirmation or rescue this nominee through a
longer horizon, replacement seed, endpoint switch, or bank/order change.

The [closed ledger](ledger-after-cc01.json) charges 601,721,522 admitted frames,
including 3,326 stop-drain frames, and 1,583,144 known auxiliary frames. Resumed
totals are 2,982,484,247 admitted search and 67,878,337 known auxiliary frames.
Setup, unadmitted work and any out-of-job reconstruction remain unknown. No
boss/Wily improvement or untouched validation has been established.

Recompute the failed confirmation without emulation:

```sh
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cc01-output --pair 0 --prefix cc01 --registration-commit e3931c94518f99b20500ef0dc41bf235a5e6c3fd --out /tmp/cc01-pair-0-verification.json
python3 benchmarks/search/continuation-reassessment/verify_cd01_native.py --protocol benchmarks/search/continuation-reassessment --evidence benchmarks/search/continuation-reassessment/cc01-output --pair 1 --prefix cc01 --registration-commit e3931c94518f99b20500ef0dc41bf235a5e6c3fd --out /tmp/cc01-pair-1-verification.json
python3 benchmarks/search/continuation-reassessment/score_cc01.py --registration benchmarks/search/continuation-reassessment/cc01-registration.json --evidence benchmarks/search/continuation-reassessment/cc01-output --out /tmp/cc01-analysis.json
```

## Offline decision after failed confirmation

An [existing-evidence opportunity audit](prefix-opportunity.md) rejects ordinary
duplicate filtering and retained-prefix fast-forward as the next experiment.
Full-prefix filtering already exists, and all twelve native cells record zero
in-job origin reconstruction. Under an explicit independent-draw model, a
time-uniform bound limits ideal partial-prefix work savings on the six controls'
original histories to about 0.17% of admitted frames. The exact finite
inequalities and an assumption-breaking counterexample are executable. This is
neither measured savings nor a PRNG certificate or adaptive discovery bound.
No new instrumentation, emulator work or policy promotion follows.

A [bounded Fable 5.1 xhigh review and adjudication](post-cc01-adjudication.md)
then identifies a distinct representation question: whether lower classified
boss HP is lost in a retention collision. A subsequent source check disproves
the proposed separate within-job same-key suppression: `previous_key` supplies
completion context, and Metroid's completion is the identity. The existing
retention hook sees every ordinary full-slot different-input competition. The complete
AP01 stream has 3,295 rejected candidates but no endpoint HP records, so the
saved evidence cannot answer that query. The next task is a minimal replay-only
design for that exact short pilot, with explicit cross-host compatibility and
unchanged per-job digests/decisions. No new policy panel or replay allocation is
registered yet; the review's automatic 150M-frame proposal is not adopted.

RR01 is now separately registered for 2026-09-10 17:51:24–18:11:24 UTC, with
synthesis reserved through 18:31:24. It uses the compiled ms02 replay caller,
first qualifying four original jobs, then conditionally inspecting the complete
AP01 stream and original final raw checkpoint. The 60/300-second intrinsic
watchdogs, 450-second service, 4 GiB memory and 1 GiB output bounds accompany
500k/750k prospective physical frame ceilings; no fresh search is allocated.
The [frozen query](rr01-query.md) covers both rejected candidates and displaced
incumbents, distinguishes resource equality, and keeps unavailable observations
unknown. Any failure stops without automatic retry. Qualification then failed
at its first root restore: the saved snapshot embeds the original ARM core
hash, which the ms02 build must reject. It consumed 0.201 seconds wall; full
inspection and the local-loss query remain unrun. This was an avoidable
preflight error, not evidence against the retention hypothesis. The corrected
caller rejects cross-build raw replay before constructing a target. A positive
future diagnostic would still require the issue #281 paired continuation
counterexample before changing a policy.

The [closed failure analysis](rr01-closure.md), exact native artifacts and
[ledger](ledger-after-rr01.json) preserve the failed registration and separate
929 source-inferred setup frames from missing runtime counter receipts. No
replay job ran. Cumulative resumed known auxiliary work is 67,879,266 frames;
admitted search remains 2,982,484,247. Verify this closure without an emulator:

```sh
python3 benchmarks/search/continuation-reassessment/verify_rr01_failure.py
```

[RR02](rr02-registration.json) separately registers action reconstruction after
the [source contract](rr02-source-contract.md) passes ten Rust tests, strict
Clippy and six offline query/accounting/comparison tests. The exact ms02 build
completed successfully. The 18:38:48–18:58:48 UTC allocation first reconstructs
the same searched prefix and qualifies four original jobs; only a pass permits
full reconstruction, replay and capture. Qualification/inspection use 90/300-second
watchdogs and 650k/900k prospective physical ceilings, under a 510-second, 4 GiB,
four-CPU, 1 GiB-output service. No fresh search is allocated. These are ceilings,
not spent frames. The complete [query](rr02-query.md) is unchanged but explicitly
applies to the reconstructed ms02 campaign; historical discarded-state identity
is not established by equality of the known artifacts. Any failure stops without
retry. RR01 and all failed policy gates remain closed.

[RR02 completes both stages](rr02-results.md): the known root/checkpoint
correspondence and all original job results pass, with 3,770 complete competition
observations. The frozen query finds 22 distinct lower-HP rejected states with
equal measured player resources among 1,717 comparable events; 2,053 unclassified
events remain unavailable. The earliest pair is HP 137 versus 140 at
competition 692/execution 423. Its other state differences remain visible, so the
next decision is a paired continuation counterexample, not a policy promotion.
The two phases take 2.804/8.612 seconds and charge 488,740 known auxiliary frames.
The [closed ledger](ledger-after-rr02.json) and compressed native evidence are
verified by `python3 benchmarks/search/continuation-reassessment/verify_rr02.py`.

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

The frozen RR02 pair is now exported without emulation in `pc01-pair/`; all3770
records and four selected payload hashes pass. [PC01](pc01-design.md) specifies
a32-tail comparison of the actual complete states, counting new HP loss from
separate fresh baselines and retaining both directions and unavailable intervals.
The source probe verifies every measured boundary by a held-action replay.
Native work requires a separate published registration; the exported local
inputs are not boot-replay witnesses.

[PC01 completed](pc01-results.md):6/32 fixed tails have a strictly better new-damage
continuation from the discarded complete state under equal-resource comparison;
none meets the reverse criterion. Both arms die in all32 episodes and neither
defeats a boss. All242 measured boundaries reproduce under held-command replay.
The0.602-second diagnostic spends15,583 measured auxiliary frames and closes
with its complete ten-file evidence bundle. This earns a retention-design
assessment, not a scalar-HP claim or automatic policy panel. The full goal is
still unachieved.

[PG01](pg01-design.md) implements a generic ordinary-anchor plus qualified-progress
alternate, and a capacity control with the same eligibility but ordinary ranking.
The opt-in Metroid scope includes exact equipment/capacity/boss identities as well
as qualified boss identity, preventing equal item/tank counts from hiding a
resource tradeoff. PC01's actual exported inputs exercise the changed production
rule without emulation. Source checks pass in default, new-feature and relevant
combined-feature builds; the eight logs and commands are in `pg01-source-checks.json`.
The archive-challenge caller can pin the projected root and explicit policy.
No native qualification, conditional policy panel or fresh-search improvement
has yet been measured for PG01.
