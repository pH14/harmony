<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Famidash escape audit

This workflow finds reproducible escape candidates in the official Famidash
1.2.8 ROMs and prepares reviewed level-data patches. It shares the arcade's
QuickNES loader and clearing routes, with an independent FCEUmm verifier.
It does not change the arcade's game rules, hide escaped states, or infer a
bug from a map pixel. Geometry comes from the actual level source and its
metatile collision definitions.

The original `audit.mjs scan` uses custom input mutations, **not Dissonance**.
For actual arcade-engine searches, use `soak-level.mjs` through `full-round.py`.
These call the Rust Dissonance planner and worker; they do not substitute a
custom input mutator. The older `dissonance.mjs` drives campaign searches.
The [investigation package](findings/dissonance/README.md) retains reviewed
reproductions and the withdrawal of the early-level tile proposals.

The initial Stereo Madness, Base After Base, Back on Track and Cycles
interior paths include intended hidden coins. Background fill alone cannot
establish forbidden terrain. Always review sprites and alternate objectives.

## Complete-level Dissonance searches

The focused WASM API exports `famidash_level_search` and
`famidash_level_worker`. It selects one level, accepts an ordinary-input
checkpoint, and continues searching after finding a victory. The production
campaign API keeps its existing objective and stopping behavior.
`check-search.mjs` compares the default stream with an unmodified branch
build and checks selected-level retention, budget exhaustion and replay of
exported victory inputs.

```sh
python3 demos/twin-dragons/tools/famidash-audit/full-round.py \
  source.json ROM_DIRECTORY RUNTIME_DIRECTORY BINDINGS_JS SEARCH_WASM OUTPUT \
  --round 1 --workers 4
```

Each level starts with 50,000 genesis executions and 5,000 per 1,024-frame
checkpoint. `FAMIDASH_MIXTURE=energy_splice:6` selects another Dissonance draw
mixture; `FAMIDASH_PREVIOUS_ROUND` adds earlier frontier and clearing inputs.
`FAMIDASH_MENU_DELAY=3` waits three ordinary released frames in level select
before entering the level. This varies the game's RNG without writing its
state: changing only the search seed does not vary a fixed genesis RNG.
Reports, candidate inputs and replay witnesses retain that setup.
Use `FAMIDASH_STARTUP_FRAMES=180` for new random-mode coverage: both cores
then use the same cold-start wait. The original release QuickNES witnesses
use a legacy 22-frame wait and their independent FCEUmm profile uses 180;
those profiles do not establish RNG equality in random-mode levels.
New witnesses record their waits explicitly. FCEUmm restores its serialized
cold power-on state before every replay, so SRAM and RNG left by an earlier
replay cannot change a later control. The randomized setup regression checks
a newly discovered Dissonance input in both cores. In Lucky Draw, different menu waits expose different
random modes and death sequences. Rotate these setups across full rounds;
a fixed menu timing leaves reachable random-mode coverage gaps.
Reference routes are freshly replayed. Only their ordinary prefix before
assistance, death or completion can seed a search. New victory inputs add
checkpoints even when the reference route is unusable. `FAMIDASH_ROOT_PLAN`
can supply a nonempty JSON array of `{input, trim, budget}` for focused gap
closure. Input prefixes are replayed before they become search roots.

The ledger records executions, frontiers, clears and candidates separately.
A searched level is not a reviewed level. `review-round.mjs` verifies saved
endpoints and inventories entry signals, including both players in dual
mode, partial hazards, persistent solid overlap and horizontal escapes.
`boundary-corners.py INDEX OUTPUT` inventories shared spike stair patterns and
the sprites in their fill components for intent review. It never edits levels
or classifies topology alone as a bug.
`components.py` groups connected decorative fill across visual styles for
source review; schema 3 records index/review hashes and sprites whose origin
tile belongs to each component. Sprite extents are not tested. Detector
signals never automatically become bug reports or tile edits. Repeated mechanical
states retain the last collision sample: a lag frame must not invent a new
horizontal sample. Detector revision 7 records physical collisions hidden by eight-bit centre
wrapping, and corrects lag, discontinuities, and the
second player's distinct collision phase and wave height.
The standard Famidash target bounds rollouts after the level-length table;
the missed-finish signal therefore begins 64 pixels past the source edge,
before that search cutoff. Review extends ordinary input in the real core to
check whether the player bypassed the finish trigger. The search cutoff is
not a game death or a successful clear.

The current investigation's stopping goal is all 109 installed levels
assessed, every candidate disposition recorded, and three consecutive full
rounds on a stable repaired build without a new confirmed bug. A new bug
resets that counter. Bounded searches cannot prove absence of every bug.

`export-control.mjs` exports fresh Dissonance clearing controls with their full
traces. `export-witness.mjs` retains ordinary failing input and a full fresh trace;
`dissonance-replay.mjs` and `fceumm.py` verify it in separate emulators.
Witness export validates the saved endpoint for the candidate’s selected player,
including player two in dual mode; its full trace records the primary game state. A
repair must kill at the reviewed escape, preserve the approach, and precede
any later original death. Fixed campaign checks also require death to remain
committed and the level to reset within 180 released frames. `compare-controls.mjs` records completion and coin
collection for paired reference, discovered clearing and retained coin paths.
A timing divergence needs investigation; an old route can itself exploit a
bug and is not automatically an intended-playability requirement.

For newer upstream builds, `symbol-runtime.mjs` reads the build's DBG symbols
instead of assuming the release RAM map. Set `FAMIDASH_SYMBOLS` when searching
or replaying that build, including `render-entry.mjs` when capturing a frame. Its Dissonance adapter translates read-only target
observations; gameplay snapshots and controller execution remain native to
the ROM. The only RAM write is the normal level-select setup. Witnesses
record their symbol layout and are independently replayed with FCEUmm's
symbol-based decoder. This adapter has been exercised on the shared Time
Machine, Theory of Everything and B-side Fofii levels, not every current-upstream mode.
The adapter checks the selected level name against the ROM pointer table.
Current upstream reordered B-side levels: Fofii is index 10, release index 6.
An index entry may specify `release_target_level` to use the corresponding
release target length/progress model; only adapter observations and the normal
level-select setup translate that alias. Recorded gameplay uses the actual level.
`trace-collision.py` uses export-only FCEUmm CPU hooks to observe the original
ROM's collision samples without modifying the game.
`trace-mesen-collision.py CASE CORE ROM DBG OUTPUT --start N --end N`
provides the same schema through the retained read-only Mesen opcode hook,
including HUGE ROMs that FCEUmm maps incorrectly. Build the core with
[the opcode hook](findings/shared-engine/wave66-shared-tile-reads/mesen-opcode-hook.patch).
It snapshots probe RAM before a collision load, records the actual accumulator
result at the following instruction, and tracks mapped JSR return addresses
for caller classification. Unknown callers, unsupported instructions and
unresolved reads fail the check. It independently verifies the ordinary-input
prefix and retains ROM, core, case and symbol hashes. Use short intervals for
instruction callbacks; source geometry and route intent remain separate checks.
`review-centre-entries.py INDEX CASE CPU OUTPUT` binds a schema-2 bank-aware
trace to its exact case, ROM and source index. It groups actual raw centre reads
by source region, separates live entries from contacts on a committed death
frame, and retains source mismatches, outside-source samples and nearby
objectives. Both players are included. Its output leaves source review pending:
raw reads still need mode-specific geometry and route-intent assessment.
Use `--regions-index REFERENCE` to retain an original region's footprint after
source edits replace or split it. Actual reads still validate against the
repaired source and ROM; the reference supplies only region geometry. Level
name, dimensions and origin must agree. This prevents a repaired region from
silently disappearing from the review because it is no longer decorative.

`verify-geometry.py INDEX ROM_DIRECTORY BUILD_DIRECTORY OUTPUT` decodes the
ROM's actual level-pointer tables, LZ streams, RLE runs and continuation IDs
using matching DBG file offsets. It compares every decoded tile with the
source index instead of assuming that a source checkout describes a ROM.
It also compares all 256 compiled collision classes against the index, and
checks named level pointers when present. `level_header_bytes: 14` selects
the current upstream header; the release default is 18.
The official release's 3,726,087 tiles across all 109 levels match the index.
The D-Sides source/ROM byte discrepancy is stale continuation metadata, not
a geometry difference; the retained [shared findings](findings/shared-engine/README.md)
explain its reproduction and cache-key repair.


`check-pit-plane.py` compiles the proposed C pit-crossing helper and compares
its geometry against an independent exact rational oracle, including upward
and downward crossings, diagonal tile edges and 240-pixel room boundaries.
This does not replace NES replay, bank-size or frame-budget validation.
`check-centre-carry.py ROM DBG OUTPUT` executes the assembled centre-coordinate
calculation against a linear oracle, including both carry and 240-pixel room
boundaries and the actual mini/normal hitbox heights.

## What is generic

1. **Export the release geometry.** `source.py` reads all 109 levels in ROM
   order from the generated level tables, including split-level references,
   reordered Tiled tilesets and bottom-aligned world coordinates. It requires
   a clean checkout at the release revision. It never runs upstream Python
   modules or their automatic package installation.
2. **Establish a control and review its legality.** Play the checked-in clearing route
   without debug/invincibility assistance or map-builder palette writes. It
   must reach the actual level-complete state. Missing, assisted, dying and
   incomplete routes are reported as unassessed. Completion alone does not
   establish that the route stays inside the intended course.
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
attribution needs an additional ROM-data check. The older custom detector cannot find every kind of glitch and excludes
dual states; the focused Dissonance inventory inspects both players.

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
requires the recorded emulator binary. The release decoder uses its pinned RAM map; newer upstream campaign
witnesses require their recorded symbol layout and matching DBG file.

## Reviewed patch generation

The Stereo Madness and Base After Base tile proposals have been withdrawn:
they blocked intended hidden-coin routes. Their historical inputs and
experimental patches remain in `findings/dissonance/`, with the correction
and collection evidence documented there. A successful replay or unchanged
clearing control is insufficient to classify decorative fill as forbidden.
Source review must include sprites, coin objectives, and alternate routes.

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
The generated manifest describes the source edits. Current B-side export,
build, original/fixed independent replays and a fresh repaired clearing control
are now retained in the [shared findings](findings/shared-engine/README.md).
B and HUGE both export and build successfully. HUGE replay and broader
playability review remain separate. This proposal
has not been submitted.

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

`--finish-fixed` validates a completion repair instead of a death repair. It
requires an ordinary-input campaign witness whose original trace survives
past the reviewed missed trigger. The repaired replay must preserve the
mechanical approach and change the final game state to completion near that
trigger. It is mutually exclusive with `--fixed`.
`check-finish-gate.py ROM DBG OUTPUT` executes the actual assembled offscreen
sprite gate for all 256 types, 16 slots, and three animation states. It checks
that the end trigger stays active vertically and preserves animated coins.

The current HUGE ROM exceeds QuickNES's supported PRG size; FCEUmm also maps
its fixed bank incorrectly, including on an unmodified upstream build.
Use a Mesen libretro core with `fceumm.py --driver mesen` for independent
verification. `mesen.py` installs callbacks in Mesen's required order and
provides isolated temporary system/save directories. The pinned Mesen revision
and core hash are recorded with the findings.

`replay-control.py CASE CORE ROM OUTPUT --driver mesen --symbols DBG --level N
--side main` replays an existing ordinary input on another build, retaining
actual outcomes and every differing mechanical frame. Omit `--symbols` for
release witnesses. This tool records a positive control only when it actually
clears; it neither discovers inputs nor claims strict trace identity. Its
comparison excludes the level ID to support moves between standard and HUGE
sets. Failed attempts are retained as rejected controls.

`verify-geometry.py` reads the PRG bank count from the iNES/NES 2.0 header,
so the same compiled-geometry check supports standard and HUGE layouts.
`check-vertical-motion.py` also checks generated bank-dispatch arguments and
executes the selected arithmetic kernels. It does not emulate the mapper;
independent NES execution remains necessary.

`trace-probe-history.mjs CASE ROM DBG RUNTIME START END OUTPUT` replays every
ordinary-input witness frame and asserts exact trace, ROM and core identity.
It retains both players' collision-history bytes and sweep scratch values in
the selected interval, with case and debug-symbol hashes. These are engine
history samples; scratch values can be stale and the output is not an
independent geometry oracle. Use it to investigate collision-phase versus
post-frame position estimates, as in the retained C5 top-base false positive.

Schema-3 components include sprites whose origin tile belongs to the region.
The boundary inventory includes stair corners on source edges and rounded
hazard/fill adjacencies with component references. These are review leads,
not bug classifications or instructions to modify every listed cell.

`check-wave-slope-point.py ORIGINAL REPAIRED OUTPUT` host-compiles the actual
experimental LU45 helper, asserts the body slope routine remains identical,
and exhaustively checks point geometry and probe-state restoration. It does
not test NES banking or renderer alignment. `check-slope-context.py` belongs
to the rejected earlier raw-center experiment and is retained for comparing
that hypothesis; a passing oracle does not make that experiment acceptable.

Schema-4 boundary inventories also list quarter-block/fill adjacencies. A
quarter block cannot seal an entire tile edge; actual input and intent review
are required to distinguish a floor gap from intentional open geometry.
The single-level runner looks up reference inputs by the release-side/level
alias, allowing current fifth-set levels to use their original workload
identity. Physical selection, symbols and ROM identity remain current.

`review-control-regions.py INDEX HISTORY OUTPUT --side SIDE --level N
--seed COLUMN ROW [--seed COLUMN ROW ...] --aligned-wave` checks complete
verified probe history against explicitly selected fill components. Add
`--linear-y` for current upstream's linear-coordinate history. It checks
source/metatile agreement and reports visits without declaring region intent.
Normal wave/Snake controls can include both biased and aligned centres.
Schema-2 histories record both players' world positions and check their
region visits independently. Legacy dual histories lacking both positions
are rejected. A clearing input that visits a reviewed
forbidden region is unsuitable as a playability control even if it wins.

Use `review-control-regions.py --regions-index REFERENCE` to preserve reviewed
component footprints when caps split a region. The actual index still
validates recorded metatiles; both index hashes are retained.

`slope-edge-masks.py SOURCE OUTPUT` host-compiles the original slope function
and measures normal-cube point coverage for all 20 slope classes. Pass its
output to `boundary-corners.py --slope-masks MASKS` to annotate schema-5
slope/fill leads with actual edge masks and solid-pixel counts. The source
revision must match the index. Full edges can still be crossed between
samples; partial edges may be intentional. Neither result authorizes edits.

`triage-closed-components.mjs COMPONENTS DISCOVERY_INDEX ORIGINAL_ROMS
REPAIRED_INDEX REPAIRED_ROMS RUNTIME OUTPUT` replays one longest-surviving
representative for every closed, sprite-free fill component on both the
original and repaired release ROMs. It retains original ordinary-input
traces for cases that survive the discovery frame. Input and index hashes
are checked; results are an inventory, not bug verdicts. Different builds
can introduce lag, so an unchanged input dying earlier does not establish
that a region is safe or unreachable. Source intent and actual CPU probes
remain separate checks. The tool targets the installed release RAM layout.

`trace-collision.py` now records instantaneous scroll X, world probe X and the
signed offset from the player origin. Use these probe-time coordinates when
comparing CPU map reads with source geometry; post-frame scroll may differ.
JavaScript callers unwrapping a low byte must normalize negative remainders
explicitly. A source mismatch inferred using a negative `%` result is invalid.

`check-spider-bases.py SOURCE ROM DBG INDEX OUTPUT` host-compiles the actual
bounded directional crossing function (4,928 cases) and executes the assembled
packed metadata lookup (256 cases). It binds source, lookup metadata, ROM,
symbol and index hashes. It does not emulate banking or establish a frame budget.

`retime-control.mjs CLEARING_CONTROL ROM RUNTIME OUTPUT.json.gz` accepts only
retained clearing controls. Set `FAMIDASH_SYMBOLS=DBG` for current upstream.
It preserves every mechanical transition after the 40-frame intro and changes
only the duration of identical consecutive observations. Any other change or
death rejects the conversion. This produces an ordinary controller tape for
positive playability checks; it cannot validate a failing witness or justify
an unchanged-input claim. Independently replay the generated input and report
added lag frames separately.

Collision trace schema 2 classifies callers by their mapped PRG ROM offset,
using `Page`, `PRGptr` and `PRGsize` exported from the trace core. CPU addresses
alone are insufficient when banked helpers occupy the same address range.
`fceumm-trace-exports.T` supplies the required linker exports; relink the same
FCEUmm objects with `make -f Makefile.libretro TARGET=fceumm_bank_trace.so
SHARED='-shared -Wl,--version-script=/absolute/path/fceumm-trace-exports.T
-Wl,-no-undefined'`. This exposes mapping data without changing emulator logic.
The tool rejects cores missing those exports. Retained schema-1 traces still
contain actual map reads, but their centre labels need bank-aware reassessment
when queries originate from switched banks. The retained frame790 caller
regression demonstrates two intermediate queries previously counted as centres.

The retained [Blast Processing cavity](findings/shared-engine/blastprocessing-cavity/README.md)
and [Dark Paradise floor](findings/shared-engine/darkparadise-floor/README.md)
packets contain narrow source-only data proposals, native release/current
reproducers, independent collision evidence and paired clearing inputs. Blast
changes a nonfatal contact before death; Dark passes unchanged-approach death
checks. Their manifests describe tested scope. Other fill regions and complete
route intent remain under review; neither packet supplies clean-round credit.

`check-wave-slope-segment.py SOURCE OUTPUT` checks the experimental normal-wave
45-degree continuous triangle routine against 46,139 exact rational clipping
cases. When the bounded signed product helper is present, it also checks all
65,536 signed byte products. Continuous tile triangles use outer coordinates
0..16. This verifies host arithmetic and geometry, independently of dispatch,
NES mapping, actual pixel intent, and frame-budget effects. The source remains
an experiment until those checks and route assessments pass.

The single-level runner retains the first clearing input from every search root
as `clearing-root-INDEX-FRAME.json`, referenced by that root's `clearing_input`.
The primary `clearing-input.json` and checkpoint scheduling still use the first
campaign victory. Per-root inputs allow separate route assessment after an
initial clearing route is rejected. Export and independently replay each chosen
input; a victory alone does not establish that its route stays inside terrain.

`check-mini-wave45.py SOURCE OUTPUT` host-compiles the experimental mini-wave45
triangle kernel and compares 47,426 motions with independent rational clipping.
It also checks all 1,024 stationary points against the original four slope
predicates and all 65,536 signed products. The cases include the Fingerdash
diagonal-intermediate crossing: both endpoint tiles are empty, but the motion
crosses a solid slope in an intermediate tile. Pixel centres preserve the
original point masks. These checks cover geometry; ordinary-input NES replays
must separately establish dispatch, banking, timing and legitimate routes.
The [isolated mini-wave45 experiment](findings/shared-engine/mini-wave45/README.md)
retains the original Fingerdash reproduction, unchanged-input repairs in
three emulators, pristine-source patches and mode-specific clearing checks.
The [shared-read integration](findings/shared-engine/mini-wave45-shared-tile-reads/README.md)
retains combined regression checks, fresh clearing routes, bank/asset layout
checks and the current negative cases that remain unqualified.

`review-mini-wave45-motion.py INDEX CASE CPU OUTPUT` checks consecutive actual
centre probes against exact source triangles, including diagonal intermediate
tiles. It binds the case, ROM and bank-aware CPU trace, checks source reads,
keeps each player's history separate, and distinguishes surviving crossings
from death contacts. It reports exact parameter intervals. This catches paths
whose endpoint tiles are both empty, which a slope-to-fill transition scan
misses. It excludes mode/size discontinuities and motion over 16 pixels;
teleport history and source intent still require review. A result is a lead,
not permission to fill a source component.

`check-mini-wave66.py SOURCE OUTPUT` extracts the actual mini-wave66 polygon
kernel and checks 64,850 cases against independent exact rational clipping,
plus all 65,536 signed byte products. The retained
[isolated mini-wave66 proposals](findings/shared-engine/mini-wave66/README.md)
include pristine-source patches, opposite-direction negative cases, independent
NES replays and mode-specific clearing controls. Host arithmetic checks do not
qualify NES timing, history invalidation or all intended routes.

`check-mini-wave22.py SOURCE OUTPUT` checks all eight experimental mini-wave22
polygons against independent exact clipping (74,850 motions), the actual
source's 2,048 point results, and all 65,536 signed products. The conservative
sweep's stationary mask must be a subset of the original mask; the unchanged
discrete checks preserve the original boundary pixels. This avoids enlarging
the stationary hitbox while detecting paths through visible solid geometry.
It includes the actual Dorabae Basic 10 diagonal-intermediate crossing.
NES timing, bank dispatch and legitimate routes require separate replay checks.

The [both-size wave66 experiment](findings/shared-engine/wave66-both-sizes/README.md)
extends that kernel to normal wave and invalidates history on size changes.
It includes a fresh original-release normal-wave reproducer, unchanged-approach
independent death checks and explicit rejected-route evidence. Current native
normal-wave reproduction and broader integration remain open.

`check-wave66-dispatch.py SOURCE ROM DBG OUTPUT` executes the assembled
collision-history query gate against 131,072 combinations of old/current
classes, mode, crossed axes, player and size. Its independent decision rule
preserves generic spike queries, known steep-slope boundary crossings and
diagonal crossings with empty endpoints. It checks the actual ROM table and
rejects unsupported opcodes or unexpected callees. This checks dispatch
coverage; the motion kernel, mapper and frame budget require separate checks.
It also checks the shared tile helpers' selected direct-call edges against
physical 8 KiB PRG banks. Equal CPU addresses do not establish that a banked
callee is mapped. Separate linker segments can share a physical bank; the
standard and HUGE layouts must both pass this check.
When the integration contains `mini_wave45.h`, the same assembled check also
requires the four 45° classes to dispatch only for mini-wave. Steep slopes
continue to dispatch for both sizes. It executes the actual flag-table gate
for all 131,072 combinations, including its accumulator shift and carry flag.

`check-bank-assets.py ORIGINAL_BUILDS REPAIRED_BUILDS OUTPUT` requires sound
effects and both PCM sample segments to preserve their bytes, CPU addresses
and physical ROM offsets in every built layout. `--segment NAME` selects
other explicitly reviewed assets. Equal sample bytes at a different address
are insufficient when metadata records only a bank. Integration caught this
failure when a new routine preceded `PCM_BANK`; placing the routine after the
sample segment preserves the original layout. Asset checks do not replace
mapper and playback validation.

The [shared tile-read integration](findings/shared-engine/wave66-shared-tile-reads/README.md)
uses each queried tile for both spike and steep-slope checks. It retains the
timing regression that motivated the merge, ordinary-input regression checks,
current/HUGE positive controls and the bank-placement failure caught during
integration. Compilation and logical CPU addresses alone do not qualify a
banked helper call.

Source-component schema 4 lists coins within eight tiles of each bounding box
as `nearby_objectives`. This is proximity, not a reachability or intent verdict.
An empty `sprites` list only describes membership: a closed component can form
part of a route to an objective outside it. Electroman's mini-ball corridor
leads to Coin 3 seven tiles beyond its two-cell component. Preserve independently
verified coin routes when reviewing proposed caps.

For a warm search root, `retime-control.mjs` accepts `--prefix-frames N` from a
full clearing control. It preserves the same mechanical-transition invariant
and emits `purpose=positive-control-prefix`, with source-prefix and full-clear
lengths recorded. A prefix is not a completion proof or a failing-witness
conversion. Independently replay it before using it as a Dissonance root.

Before a current or HUGE ROM search, run `check-target-profile.py INDEX
RELEASE_INDEX SIDE LEVEL` against the full pinned 109-level release index.
Geometry indices retain physical slots; a search may need explicit
`release_target_side` and `release_target_level` aliases. Current Hell D8 maps
to release C8. Defaulting it to D8 selects Slaughterhouse's profile and can
truncate exploration. The check compares source names, reports matching release
slots and binds both index hashes. It does not establish equivalent geometry
or playability. Keep the running campaign's runner and WASM frozen; run this
separate check before launching a new focused search.
