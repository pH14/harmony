<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Famidash escape audit

This workflow finds reproducible escape candidates in the official Famidash
1.2.8 ROMs and prepares reviewed level-data patches. It shares the arcade's
QuickNES loader and clearing routes, with an independent FCEUmm verifier.
It does not change the arcade's game rules, hide escaped states, or infer a
bug from a map pixel. Geometry comes from the actual level source and its
metatile collision definitions.

## What is generic

1. **Export the release geometry.** `source.py` reads all 109 levels in ROM
   order from the generated level tables, including split-level references,
   reordered Tiled tilesets and bottom-aligned world coordinates. It requires
   a clean checkout at the release revision. It never runs upstream Python
   modules or their automatic package installation.
2. **Establish a normal-play control.** Play the checked-in clearing route
   without debug/invincibility assistance or map-builder palette writes. It
   must reach the actual level-complete state. Missing, assisted, dying and
   incomplete routes are reported as unassessed.
3. **Search deviations.** Seeded input variations branch from control-route
   checkpoints. The first detector looks for a player more than 32 pixels
   below the nearby control route, inside source tiles `X_NO_OUTLINE` or
   `BLACK_NONE` with `COL_NONE`. It requires another 150 surviving frames,
   at least 120 inside the suspect region, and at least 64 pixels of forward
   progress. Paused, dual, debug and invincible states are excluded.
4. **Save a witness and replay from the beginning.** A candidate contains
   ordinary controller input, the complete mechanical trace, its clearing
   control, geometry, source/ROM/core hashes, detector parameters and seed.
   A fresh replay must reproduce the endpoint. Snapshots accelerate discovery
   but are not needed to reproduce the finding.
5. **Check another emulator and review intent.** `fceumm.py` compares the
   recorded mechanical fields frame by frame and replays the clearing control.
   This establishes actual game behavior. Source review still determines
   whether a lower corridor is intentional or a bug.
6. **Prepare the appropriate fix.** Missing pit hazards can use a local tile
   patch. Crossing tiles that already have hazard/solid collision needs a
   collision-engine investigation. Coordinate discontinuities need a separate
   arithmetic oracle. Do not globally make decorative tiles solid or add a
   universal y cutoff based on this detector.
7. **Validate before proposing upstream.** A level-data patch must kill the
   escaping input near the reported region, preserve its mechanical approach,
   and preserve the clearing control's entire mechanical trace and completion
   frame. Re-export and build every affected ROM, check bank capacity, and
   repeat the independent emulator check. Engine changes need a broader
   cross-mode suite and NES frame/cycle-budget checks.

The source geometry is a triage aid, not an executable collision oracle:
partial blocks, slopes, dynamic objects, small gaps and deliberate secret
routes need review. The pinned D-Sides source differs from its release ROM
in 28 level-data bytes, as documented by the workload; source-level D-Sides
attribution needs an additional ROM-data check. This first detector cannot
find every kind of glitch and does not search dual states.

## Commands

Requirements: Node, Python 3 (standard library only), a clean Famidash release
checkout, the four pinned ROMs, and the arcade QuickNES runtime. The runtime
directory contains `engine/quicknes.js` and `engine/quicknes.wasm`.

From the repository root:

```sh
python3 demos/twin-dragons/tools/famidash-audit/source.py FAMIDASH_SOURCE source.json
node demos/twin-dragons/tools/famidash-audit/audit.mjs scan \
  source.json ROM_DIRECTORY RUNTIME_DIRECTORY OUTPUT_DIRECTORY \
  --trials 500 --seed 1
```

Use `--side main|b|c|d` and `--levels 5,6,7` to narrow a run, and
`--max-findings 2` to retain more than one candidate per level. Nearby
discoveries are coalesced in 256-pixel horizontal regions. `report.json`
records candidates, budget-limited misses and explicit coverage failures.
Witnesses are compressed JSON files; no ROM bytes are included.

```sh
node demos/twin-dragons/tools/famidash-audit/audit.mjs replay CASE.json.gz ROM RUNTIME_DIRECTORY
python3 demos/twin-dragons/tools/famidash-audit/fceumm.py CASE.json.gz FCEUMM_CORE ROM --output independent.json
```

QuickNES and FCEUmm can round x differently during the entry animation in
some levels. Strict comparison is the default. An explicit `--intro-frames
40` excludes only those first 40 frames; the result records the comparison
start and every differing entry frame. All later witness and control fields
must match. The retained Select Payment Type witness differs at entry frames
2 and 6, and Retray at frame 3; their active gameplay matches from frame 41.
Neither failure is silently accepted as an exact whole-trace match.

For patched ROMs, append `--fixed` to either replay command. QuickNES still
requires the recorded emulator binary. These verification commands target
the pinned release's RAM map; a newer upstream ROM needs its symbols checked
before using the same decoder.

## Reviewed patch generation

`patch.py` takes a JSON specification with an exact target revision, a hash
of each reviewed source file, and explicit old/new Tiled GIDs. It produces a
source-only patch and manifest, checks that the patch applies, and leaves
the source checkout untouched. It refuses stale files, unexpected tiles,
duplicate edits and out-of-range coordinates. Tile placement is a reviewed
decision; search output never automatically edits a level.

```sh
python3 demos/twin-dragons/tools/famidash-audit/patch.py \
  CURRENT_UPSTREAM_SOURCE reviewed-edits.json proposal.patch
```

`examples/fofii-current-edits.json` and `examples/fofii-current.patch` adapt
the tested Fofii repair to upstream commit
`541d68ffcae2096f116146ef21cbc4d62886eb6b`. Upstream now shares its level
sources under `lvlset_HUGE`; the three defective cells are still unchanged.
The generated manifest deliberately says **source proposal: export, build
and replay required**. The release ROM patch is tested; the current-upstream
ROM is not. This proposal has not been submitted.

After applying a current-upstream source proposal, regenerate each metadata
set that references the edited level. For Fofii that is B and HUGE:

```sh
python3 LEVELS/export_wrapper.py -l B
python3 LEVELS/export_wrapper.py -l HUGE
make -B b-sides huge
```

Use the upstream exporter's generated assets and bank placement. Do not
overwrite a compressed level blob in a release ROM without checking its
length and relocated pointers. Include the source diff, exporter diff,
input/control witnesses, independent replay result, bank sizes and affected
ROM identities in the proposed upstream change.

## First cross-level sample

`findings/report.json` records the 2026-10-18 run over all 109 levels with
150 trials per assessable level and seed 1. It executed 14,475 variations:
95 levels had no candidate in that budget, 10 were unassessed, and 4 had a
candidate. These are bounded search results, not a proof that 95 levels are
bug-free.

| Level | Independent replay | Source review / patch status |
| --- | --- | --- |
| B-Sides Fofii | Matches | Missing pit hazards; release patch and clearing control verified |
| B-Sides Moonlight | Matches | Survives in decorative fill below the course; tile fix still needs review and validation |
| B-Sides Select Payment Type | Matches after entry animation | Crosses a real spike-collision row; investigate shared collision sampling rather than filling decoration |
| Community Retray | Matches after entry animation | Rejected candidate: player is in a lower playable ball corridor; no patch |

The four self-contained witnesses are retained beside the report, including
the rejected example. They separate detector candidates, real reproduced
behavior, intent review and tested repairs. The existing
[Fofii regression package](../../../../workloads/nes/src/famidash/repros/fofii/README.md)
contains the release patch and its original reproducer.

The first three findings are useful regression seeds for future collision
work. Do not claim the Select Payment Type escape has the same root cause
as Fofii merely because both end at y 897.

## Checks

```sh
cd demos/twin-dragons
npm run test:famidash-audit
```

The unit checks cover assisted-route rejection, source/world coordinate
mapping, unsupported gameplay filtering, reordered tilesets, and guarded
tile edits. The actual 109-level scan and independent emulator replays are
recorded evidence, not tests that run automatically without the game assets.

The retained geometry and source patches derive from Famidash by TFDSoft,
Copyright 2023–2024 TFDSoft, under the [MIT license](LICENSE-FAMIDASH.txt).
The audit tooling itself is AGPL-3.0-or-later.
