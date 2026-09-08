<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Search experiment synthesis

The integration base is `origin/main` at `444ac54c` (including the workload
boundary refactor at `115adda4`). This is a semantic port: prototype engine files
are not merged wholesale across the old/new package boundary.

| Source | Useful evidence | Treatment |
| --- | --- | --- |
| SMB autoresearch, `cand54.patch`, source derived from `6fe85d12` | Repeatedly selected cheap states can starve costlier alternatives; three historical 24-worker fresh completions | Recover count-adjusted cost weighting under a new identifier. Recheck paired seeds, worker counts, suffix policy and memory; historical results do not establish a win on current main. |
| MM2 Claude session `cd484718-ab48-58f8-8e35-7bcb3f22170b`, adapter lineage `c6dbbf7a` and subsequent local decoder repairs | False-death corrections; paired route reuse often retained a health advantage | Port decoder and contracts; use discovered same-slot exits as the generic continuation hypothesis. Independently staged boss clears are not a whole-game solve. |
| Metroid Claude session `108e7269-155a-51f6-89de-4ec6421fb4fb`, `9e52034e`, replay experiments `4eca8c53`, `25e8f624`, `7466ccbe`, `bb9d5c95` | Cartridge RAM determinism, map-coordinate meaning, replacement propagation, multi-seed variability; half-budget replay regressed quarter-budget replay | Preserve mechanics and qualification. Build a bounded generic continuation store with a quarter reservation share. Do not import game-specific improvement tiers or compare resource preferences across unrelated locations. |
| `origin/claude/nova-squirrel-cloud-repro-mqogv1`, `6e774a96` | Whole-game terminal/unfreeze issue; isolated levels can hide whole-game stagnation | Separate level panel and whole-game case; correct intermediate-clear stopping. Fingerprint gains caused by accidental ordering and withdrawn settled-pose rules are not promoted. |
| Super Tilt Bro `codex/super-tilt-bro-luna-trial`, `0496c016` | Reproducible source build, source-labelled observations, autonomous opponent and Hard victory qualification | Port the workload and existing qualification workflow. No searcher improvement is attributed to this work. |

Private transcripts, ROMs, and historical huge archive exports are not repository
artifacts. Their conclusions are hypotheses until reproduced with the frozen
matrix. New compact reports carry their own source/executable/core/ROM hashes.

The first exploratory eight-worker panel used the current main mechanisms with
the capped suffix and the imported adapters. SMB solved 2/3 seeds at 400,000
executions; count weighting alone solved 1/3. These results reject an immediate
count-only default promotion. The historical count run used 24 workers and an
uncapped `one_to_six` suffix, so those conditions need explicit paired ablation.
Exploratory wall timings overlapped development activity and are not publication
claims. Final comparisons must use pinned builds and controlled host allocation.

Adapter audit remains separate from engine ablations. In particular, the MM2 v18 key removes the inherited rooms-visited reward and the
progress-aware selector separates location labels from progress. Metroid
count-based capability identity and ordinary splice donor ordering still need
separate ablations;
being hidden behind `ArchiveKey` does not make them unbiased. The imported Nova
and STB policies still use the default `Ord` progress relation; the semantic
parent-selection experiment does not remove their residual coordinate bias.
Source-labelled
mechanics are allowed; inferred routes, waypoint rewards, curated winning chords,
per-obstacle weapon advice, and imported solution tapes are not.

The remaining ordering audit is tracked in
[#270](https://github.com/pH14/harmony/issues/270), and the independent Metroid
capability/resource-retention ablation in
[#271](https://github.com/pH14/harmony/issues/271). Neither is silently folded
into the current fixed-policy search comparison.

The historical SMB ledger also notes that entry-local sampling counts disappear
when the memory budget drops an entry. The versioned retention-key count cache
tests that specific lifetime problem with a bounded history. It does not ban
backward movement, encode a castle loop, or add any state to a game adapter.
Its effect must be measured separately from ordinary entry counts at both memory
budgets and on games whose same-slot replacements improve resources.

## Full-panel candidate registration

After all five development pilots completed, select learned continuation replay
with the original parent selector for the full 19-origin comparison. Both arms
use the qualified two-reservation/two-result execution profile. Continuation
alone preserves every baseline pass, reduces Metal Man costs on all three
seeds, and improves Nova level 1 at the median, with a modest STB regression.
Adding entry counts or retained-key counts worsens several companion-game costs;
semantic progress increases Metroid coverage at matched work but substantially
increases Metal Man cost. They remain named experiments. The independent,
qualified 24-worker SMB reference keeps its count-weighted recipe.

`evaluation.json` is the main-mechanism control; `evaluation-continuation.json`
is the frozen candidate. Their five seeds, 20260920–20260924, are disjoint from
all development and SMB reference validation seeds. Candidate choice is fixed
before examining any completed full-panel outcome. Retain all full-panel
failures and do not retune the candidate from those validation results.

## Validation outcome

The frozen comparison completed all 190 cells without infrastructure errors.
Both arms solve 65/95: continuations gain one seed each on Nova levels 9 and 25,
but lose one SMB and one Crash Man pass. Several Mega Man stages and all STB
difficulties improve at the median; Quick Man and the previously easy Nova
fixtures cost more. Conditional victory medians must be read with solve counts.
Metroid and whole-game Nova remain unsolved at their registered budgets.

The general adoption is bounded physical overlap in the qualified native
profile, which preserves all 18 paired search streams while improving frames/s
by 32–42%. The separate count-weighted fresh SMB reference passes all five
validation seeds. Continuation replay, entry/key counts, and semantic progress
remain named experiments rather than universal defaults. Preserve the failed
cells and investigate generic causes, including the empty-bank capacity cost
tracked in [#275](https://github.com/pH14/harmony/issues/275), on newly registered
validation seeds. Full evidence and resource measurements are in
[`results`](results/README.md).

## Correction: preservation of historical depth was not established

The 005 panel is a breadth/throughput evaluation, not evidence that the deepest
historical searches were preserved. A retained Mega Man 2 power-on tape replays
to Wily 4 with all eight Robot Master weapons under the current runtime. The
panel exercised only independent Robot Master stages. A fresh chained run is
still required to test the searcher's ability to recover that depth.

Metroid's 005 budget was one sixth of the earlier 3-million-execution work budget,
with one quarter of its 8 GiB archive allocation, different worker/suffix/mixture
settings, and no historical Pareto selector or improvement-replay queue. Replaying
all ten 005 champion tapes shows Morph Ball, 10 missile capacity, no energy tank,
and Brinstar/Norfair. Retained historical champions additionally show Bombs,
Long Beam or Ice Beam, an energy tank, and Kraid-area entry. Historical aggregate
records include Ridley-area entry, but the retained champion tapes do not prove
that branch. No recovered Metroid tape verifies a boss defeat. Item counts alone had
also obscured the distinction between Long Beam and Ice Beam.

The new semantic-only selector isolates the location-neutral part of the old
Metroid Pareto behavior without importing cross-location resource preferences or
adding count weighting. It is an opt-in diagnostic, not a recovered copy of the
entire historical algorithm. The long-horizon manifests retain this distinction.
The independent Ridley flag decoder correction is versioned v8; the named-report
schema does not contribute rewards or route hints. See the progress audit for
source hashes, replay evidence, and the exact limits of the comparison.
