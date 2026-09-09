<!-- Current supplement: P05 completed in143s/2,503,117 physical frames with
6/8 useful differences in both matching and differing motion-context groups.
No capability gain, no gate reopened. Exposure-reader lower bounds are65–74%
across five verified short streams;13 Python research contracts pass.
Local b38ff2db includes the explicit Metroidv4/MM2v1 format correction.
No native experiment is running. Complete-survivor audit implementation
has an08:16UTC stop and needs stream qualification before suffix probes. -->

# Research checkpoint — September 9, R05 fails; longer campaigns stopped

The goal remains active. No fresh Metroid boss or MM2 Wily4 breakthrough has
been established. ROM transfer is complete and checksum verified. The initial
12-hour tranche ends10:16UTC;08:46UTC remains reserved for consolidation. Current
time is about07:19UTC. Do not silently extend or mark the breakthrough achieved.

No owned msr1 search campaign is running or queued. P05 is complete; the separately registered
complete-survivor audit is in implementation (see survivor-audit-design.md). R05 finished around06:14UTC;
service`harmony-r05-replication-002` is inactive. All tested retention families
have failed their registered escalation gates. Do not rescue them with longer
runs, extra seeds or altered thresholds in this tranche.

R05 used fresh development seed20261210, ordinary/context/quality sequentially
on8–11, same frozen`context-002` executable, v3 terminal, ordinary geometry,
alphabet-only,4096 actions,4 workers,8GiB,500k jobs/50M frames/1800s per arm.
All completed50M, without wall censoring. Ordinary gets missiles by17.689M,
Norfair by17.940M, tank by42.392M. Context gets missiles by45.040M and Norfair
by45.667M, no tank. Quality gets missiles by19.957M and Norfair by22.042M,
no tank. Both frozen gate comparisons fail. Context still has31,248 active
entries,14,085 distinct-context pairs, zero same-context pairs, largest slot2.
The mechanism is active; its new-seed performance is negative.

The prior R04b seed3 gain remains preserved: a replayed context tank by36.988M
against none in quality at50M. R05 does not erase that development result, but
it prevents escalating it. [The synthesis](SYNTHESIS.md), [R05 analysis](r05-analysis.json),
[full summaries](r05-results.json) and [figure](motion-development.png) show both.
Figure was rendered with Matplotlib3.11.1 in temporary environment
`/private/tmp/harmony-retention-plot-env`; PNG was visually inspected. Source
analysis hashes are in`motion-development-figure.json`.

[Updated accounting](work-accounting-after-p05.json) counts70 completed cells
and two errors,1,306,085,764 admitted frames,10,202,892 executions,19,644,104
full-replay admitted frames,2,529,008 reported witness-suffix frames and6,976,210
standalone probe frames after P05. Summed search time26,511.8s is not tranche wall time.
Setup, exports, bridges, unadmitted and incomplete work can add cost. Outputs
occupy about6.3GiB. Revised accountant is msr1 root`account-runs-v2.py`; output
`runs/work-accounting-after-r05-v2.json`. The frozen R05 accountant is older.

Draft [PR#287](https://github.com/pH14/harmony/pull/287) exists. Pushed head
`d0b794f0` includes complete R05 evidence, plot, updated synthesis and the research
CI contract integration. Its body now reports the negative replication. Local
commits`1028cd00`/`5a0eae41` register and implement P05; not pushed yet. Use
`/private/tmp/harmony-retention-pr-body.md` with`gh pr edit --body-file` for the next update.
The root pre-push hook explicitly permits skipping its convenience root checks;
changed standalone package checks already passed. Hosted CI is the gate of
record. Initial pushed head's portable job passed, broad checks still ran at
last read; PR was mergeable. Do not claim final-head CI yet.

Production implementation remains frozen at`4e3f0255`, source digest
`c0c67ce855ad874f974879bd238dd1e38d358c02d492abb04d6150c936c80113`, binary
`2a727ac12dbc4ff07272e0119bab39179aaa45bc0a6f422c2cc8fb77cdf3f981`.
All compatibility and active-context qualifications passed; defaults unchanged.
[Implementation self-review](implementation-review.md) records boundaries,
especially unqualified motion-feature splice/resume/combined-key performance.
The optional-policy fixture exercises actual alternatives, pressure and replay.
No unsafe change. Relevant portable checks are complete, plus five adversarial
R05 allocation-gate cases. Together with two chain-limit tests, the exact new
CI command passes locally in0.037s. Local telemetry reads now close their file;
the frozen R05 driver and decision semantics were not modified.

The reused [P03 objective audit](p03-objective-audit.json) finds124 versus112
exclusive map/suffix events but326 versus353 surviving endpoints. Objective
choice reverses direction on the same data. The [predictive-retention contract](predictive-retention-contract.md)
defines complete-survivor gained/lost coverage, fixed utility and sampling
assumptions, plus probe costs. No new emulator work or candidate tuning.
Its auditor passes side-swapping symmetry and rejects a planted inconsistent
exit flag. Literature memo and two later finite fixtures are committed.

P05 is registered in[p05-registration.md](p05-registration.md): all16 equal-
preference pairs from R05 ordinary's existing audit (full SHA
`d7c2e2c30741d1bd7a7a44384ee57cb1817a01c6d85e81a0fee69be218e236bd`),
recorded order, no pair-specific outcome selection. Frozen motion-probe-002
first reconstructs endpoints twice with its2M cap (predicted1,440,708 frames).
Then frozen probe-001 runs16 shared suffixes of24 actions, newseed20261214,
terminalv3. The conservative suffix-helper bound is9,621,970 frames including
all potential gain exports, below its12M registration:821,698 prefixes,
1,474,560 suffixes and7,325,712 exports. Each helper120s, whole service300s/4GiB,
64MiB output cap, core8. No retries/extra suffixes/longer run on a negative or
incomplete result. It cannot reopen the failed search family.

Active service`harmony-p05-diagnostic-001`, launched06:37UTC. Driver package
msr1 root`harmony-p05-5a0eae41/` matches committed driver hashes; wrapper
`run-p05-5a0eae41.sh`. Inputs stay private at root`p05-audit.json`. Outputs:
`runs/p05/results.json`, `runs/p05/{motion,suffix}/summary.json`,
`runs/p05-analysis.json`, `runs/work-accounting-after-p05.json`. The fixed
analyzer reports split/disagreement contingency and both objective directions,
never an inferential rate or escalation pass. Both helpers are existing attested
binaries; no production code or build changed. Metadata-only results can be
copied locally. Do not export private gain tapes to GitHub.

Remaining work: finish P05 and record its bounded result, synchronize the draft
PR, inspect final-head CI when appropriate, and consolidate the scientific result. Any genuinely new
short diagnostic requires a concrete hypothesis and budget first; no longer
retention campaign is justified by the current evidence. No validation seed
has been used. Existing follow-ups #270/#281/#283/#286 cover ordering, boss
observations, actual continuation exposure and behavioral retention.

One optional transfer remains blocked: second private Metroid development
witness, seed5, ms02→msr1. Auto-review rejected the transfer; user approval was
asked asynchronously and has NOT arrived. Do not retry or work around it.
Seed3 was approved, replayed3 times and produced an empty boss-memory trace;
E02 stays partial. The ROM permission is settled and unrelated. This optional
file does not block the other research work. Explain the remaining rejection
briefly in the final if still unresolved.

## Findings that changed the research direction

The formal model separates continuation equivalence, retention of distinguishing
futures, and finite-budget discovery. Executable finite examples use the real
archive and selector, including cases where each resource heuristic loses a
useful future and where additional retained alternatives reduce discovery.
The assumptions and proof limits are in [theory.md](theory.md).

| Mechanism | Controlled evidence | Decision |
|---|---|---|
| Joint resource-threshold coverage | Initial MM2 Metal gain exactly matched the two-extreme control. Shared-prefix Heat replication: one win, one tie, one loss at bounded work. | Failed the two-of-three gate; no longer coverage campaigns. |
| Finer Metroid retention key | Both corrected-terminal seed-3 arms completed 500k jobs. Same final named milestones; energy tank arrived about 16% earlier, below the registered 20% gate. Refined retained 161,829 states versus 42,543. | Failed the gate; no longer refinement campaigns. |
| Quality representative plus job-ranked sample | Metroid: no added milestone or20% speedup, and misses the control tank at50M frames. MM2 Metal:1.44M frames versus7.35M, both replayed. | Metroid family stopped; MM2 passed its chain-follow-up gate. |

The fresh resource-coverage chains both stopped at Heat under their declared
20-minute stage limit. Shared-prefix diagnostics do not count as fresh chains.
See [MM2 results](c01-a03-results.json) and [Metroid key comparison](k01-analysis.json)
for both favorable and adverse outcomes and conservative arrival intervals.

## New diagnostics and correctness evidence

P03 reconstructed sixteen equal-resource Metroid competitor pairs and applied
64 identical suffixes to each pair under the corrected death predicate.
Fourteen pairs had differing useful local exits or survival. The finer key split
six of those pairs and still merged eight. No new capability or boss was found.
This consumed 1,885,036 physical frames including reconstruction. The selected
sample refutes interchangeability; it is not a population-rate estimate.
[The analysis](p03-analysis.json) reproduces byte for byte from private raw output
using [analyze_p03.py](analyze_p03.py).

Two production-coordinator counterexamples show that zero parent credit at
removal does not imply an unexplored state. A state can be continued inside its
birth job, or a pending executed job can receive credit after the parent's
removal. No scheduler change is justified from the earlier zero-count fractions
alone. Follow-up [#283](https://github.com/pH14/harmony/issues/283) records the
measurement gap.

The imported, versioned Metroid BCD-underflow correction is opt-in. Legacy
semantics retain exact historical streams; new Metroid comparisons use corrected
terminal v3 in both arms. Underflow endpoints are excluded and observed in
qualification. Follow-up [#281](https://github.com/pH14/harmony/issues/281) covers
boss encounters and partial fight observations, which have not been implemented.

## Earlier R03 design and interpretation (historical)

R03 keeps the ordinary quality winner and the best state from the lowest-ranked
creation job, at most two states within the same byte budget. It uses existing
metadata, no extra search RNG and no finer key. The exact extrema invariant has
explicit fixed-stream assumptions; the deterministic rank is not a theorem of
uniform sampling in adaptive search.

The frozen default-feature ARM build is source commit `33795855`; its source
and binary digests are recorded in [the ledger](README.md). All 132 generic and
123 NES library tests, the evaluator test and strict Clippy checks passed. The
latest NES suite completed in24.29s; its persisted local log preserves the
completion lost from an earlier tool response. All five ARM qualification
cells passed full replay, including exact corrected-default Metroid compatibility.
[Qualification evidence](r03-qualify-results.json) records the actual admissions.
A checker initially demanded resource tradeoffs from the early control; its
correction reused completed cells and did not relax the candidate requirement.

The Metroid50M-frame pair completed without censoring and failed its escalation
gate. MM2 sampling found the Metal award after1,444,334 frames versus7,349,785
for extremes, about80% fewer frames. Both use the same executable, full-hold
suffix profile, selector and memory. This profile differs from C01's; only the
within-pair comparison isolates retention. [Metroid](r03-metroid-analysis.json)
and [MM2](r03b-mm2-analysis.json) preserve the full comparison and limitations.

Both MM2 witnesses report victory and death simultaneously, as the earlier C01
victories also did. The decisive continuation check is the actual stage bridge:
both R03b tapes reach Heat with28 health and the Metal weapon, replayed twice.
Sampling retains4 lives and extremes3; the exporter preserves that difference.
The first helper incorrectly demanded a living award endpoint, then was aligned
with the existing production export contract. No search predicate changed.
[Bridge evidence](j01-export-qualification-results.json) records the raw flags,
qualified endpoints and143,580 physical frames for the successful qualification.
The earlier failed helper adds an inferred16,819 frames, recorded separately.

J01 starts two new fresh chains on development seed20261102, with no
imported gameplay input. It retains R03b's full-hold profile and swaps CPU
placement: sample8–11, extremes0–3. Each stage is capped at1M jobs/120M frames
and20 minutes search; each chain stops at its first unsolved stage and has a
90-minute total limit. Later-stage comparisons are end-to-end chain evidence,
because each arm carries only its own searched prefix. Wily4 still requires
all eight weapons and twice-replayed entry, followed by untouched replication.

Sampling's new Metal victory took2,488,346 frames versus7,672,364, replicating
the earlier large first-stage improvement. It then defeated Heat at66,633,523
frames and Air at39,646,633, with healthy next-stage bridges replayed twice.
Wood stopped unsolved at72,056,120 frames and1200s. The control stopped
unsolved at Heat after40,507,046 frames
and1200s; at that common frame boundary neither had won. Consequently the
greater chain depth is promising development evidence but does not establish
a matched-work Heat improvement.

J01's completed lineage analysis passed, including every carried input hash,
twice-replayed expected stage, weapon mask and healthy bridge. Read-only planted
errors in parent lineage, a bridge weapon mask and the external-input flag were
each rejected at the intended assertion. Sampling used180,826,751 admitted
frames; extremes48,180,376. Including recorded setup/export/bridge/replay gives
physical-work lower bounds182,102,095 and48,421,033 respectively. These are
unequal completed costs, not a matched-work total comparison. See
[J01 analysis](j01-analysis.json) and [provenance checks](j01-analysis-proof.json).

H01 isolates that question with one additional extremes control from exactly
sampling's searched Metal prefix and the same seed/binary/configuration. It
reuses the completed sampling Heat result and allows85M frames/3000s on the
free0–3 cores. A20% conditional gain requires5×66,633,523 <=4× control cost
or completed frame-budget lower bound, with no wall censoring. This is a
registered diagnostic, not a fresh-chain result or untouched replication.
The original J01 chain limits and all earlier failed gates remain intact.

L01's ordinary controls both completed without censoring. Seed20261101 favors
sampling1,444,334 versus5,012,486 frames; seed20261102 favors ordinary retention
2,182,520 versus2,488,346. The two-seed production-improvement gate failed.
J02 will answer the remaining chain question with a fresh ordinary seed20261102
chain through Wood, reusing J01 sampling. Its fixed stage budgets equal the
candidate's completed stage work, with a2400s stage watchdog to allow that work
to finish and a90-minute chain cap. It cannot claim Wily4 and cannot continue
after an out-of-budget victory. Two synthetic driver-contract checks passed.
J02 is now running from committed driver56ab51ee on8–11, after both L01 controls
finished. Its Metal witness reproduces the ordinary baseline before Heat begins.

The standalone Metroid boss-memory probe also passed three replays of the D01
Kraid-area route: ordinary holds and one-frame reads end in identical emulator
bytes, with identical one-frame trace hashes. No boss loader/tag agreement was
seen along that route. This is a verified negative control, not proof about
all search routes or qualified partial-damage counters. See [E01](e01-results.json).

E02's first later development route also passed three ARM replays:107,469 route
frames and325,194 total physical frames, with zero guarded loader or active-tag
agreement. The seed-5 witness transfer was rejected by automatic approval review;
explicit approval for that private file was requested and no workaround attempted.
It remains unexecuted, so [E02](e02-results.json) is a partial two-route check.

P04 is a new, bounded Metroid diagnostic rather than an extension of a failed
campaign. It reconstructs the existing sixteen P03 pairs twice per endpoint,
reads facing and motion bytes, and checks snapshot noninterference. The coarse
descriptor and six-of-eight design gate were frozen before inspecting the new
data; existing P03 suffix outcomes are reused. Sourcef24b6b26 passed its focused
unit test and strict Clippy. The ARM `motion-probe-001` build is attested, and
the diagnostic runs on little CPU4 with2M total frames/600s/4GiB bounds. A pass
would permit specifying a memory-bounded policy and pilot, not a fresh campaign.

P04 completed and passed: the frozen coarse descriptor separates exactly six
of the eight still-merged useful pairs, including the identical-mechanical-state
pair0/trial15. It used1,587,338 physical frames, with all32 endpoints replayed
twice and all snapshot noninterference checks passing. The resulting
[context-retention design](motion-retention-design.md) calls for at most two
quality-ranked context representatives, with an ordinary top-two-quality
capacity control and unchanged selection geometry. It still needs implementation,
finite positive/adverse fixtures, replay qualification and a distinguishing pilot
before any fresh campaign is allowed.

H01 also completed: extremes from exactly sampling's Heat origin did not win
within85M frames. The conditional20% gate passed without wall censoring.
Actual admitted work85,001,033; recorded physical lower bound85,144,123. This
is an adaptive shared-prefix result against coordinate extremes, not evidence
of a fresh-chain improvement over ordinary retention. J02 remains the relevant
ordinary-retention depth comparison.

No validation seed has been used. [The validation protocol](validation-protocol.md)
separates operational repeatability, exact paired inference, censoring, and
multiple endpoints. Small negative panels and failed compute-allocation gates
do not prove an approach incapable of later success.

Work is isolated on `codex/retention-theory-msr1-20260908` at
`/private/tmp/harmony-retention-theory-msr1-20260908`, based on committed ms02
diagnostics at `8e5ae683`. The other effort's worktree and runs are untouched.
The ledger preserves each experiment's bounds, failures and implementation
history. Consolidation begins at 08:46 UTC; the initial tranche ends at 10:16 UTC.
Completing that tranche is distinct from achieving the breakthrough goal.
