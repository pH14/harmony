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
being hidden behind `ArchiveKey` does not make them unbiased. Source-labelled
mechanics are allowed; inferred routes, waypoint rewards, curated winning chords,
per-obstacle weapon advice, and imported solution tapes are not.
