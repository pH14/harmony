<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Shared collision findings and upstream proposals

These are ordinary-controller reproductions found by Dissonance. No ROM or
emulator binary is included. They are confirmed game behavior, with proposed
repairs at different validation stages. The complete-level investigation is
still in progress; no clean round has been counted.

## Downward ejection underflows unsigned Y

Time Machine (main 7, ship) subtracts a positive floor ejection from a small
unsigned relative Y at frame 5,372. The high byte becomes 253 and places the
player below the course. B-Sides level 10 reproduces the same subtraction in
wave mode at frame 1,872. Independent FCEUmm CPU observations trace the
underflow to the storing instructions, rather than inferring it from the map.

`release-ejection.patch` targets release commit
`0df680d0e02f0ccb438f1224d162a0ab073b7204`. It keeps the original subtraction,
checks the 6502 borrow, and calls a helper only on underflow. Normal downward
collision adds two cycles. The helper preserves the release's wrap, dual and
two-player exclusions. All four release variants build. Both original
witnesses retain their approach and die on the first underflow in QuickNES
and FCEUmm. Across 109 levels' paired inputs, seven timing divergences still
need broader playability review; this is not yet a shipping approval.

Current upstream commit `541d68ffcae2096f116146ef21cbc4d62886eb6b` still has
this bug. The release input dies earlier under its changed physics, so a new
Dissonance search produced `current-timemachine-wrap-witness.json.gz`.
FCEUmm reproduces its entire trace, including the entry animation. The player
remains alive below the course for another 300 released frames.
`current-ejection.patch` ports the borrow check to `currplayer_rely`; it
matches current upstream's existing non-wrap policy for both players.
All four current variants build. The repaired current witness dies at frame
5,372 with every earlier mechanical field unchanged in both emulators.
The retained current Theory of Everything clearing input is unchanged by
this arithmetic repair. A Time Machine clearing input exploits the same
underflow. A fresh Dissonance search on the repaired ROM found the retained
`current-timemachine-fixed-control.json.gz`: it clears at 5,714 with coins 1
and 3. FCEUmm matches its entire trace, including entry animation.

## Pit collision misses between frames and across narrow lanes

Theory of Everything is **main 11**, not Clutterfunk. Original-ROM CPU reads
show its centre sample at frame 1,427 at local `(6,10)` and frame 1,428 at
`(9,13)`. The bottom spike routine accepts only x 5–7 and y 11–15. Both
samples miss even though the connecting segment intersects that rectangle.
The next sample reads noncolliding fill below the pit. The original dies
later at frame 1,584; that later death must never be accepted as a repair.

A new current-upstream Dissonance witness reproduces the same sampled
coordinates and escape. Its independent FCEUmm gameplay trace matches after
entry frame 3. See the retained CPU sample and independent replay records.

Try This GD (C-Sides 7) also misses a pit: centre samples are `(7,5)` at
6,308, `(11,10)` at 6,309 and noncolliding fill at 6,310. This path passes
between the narrow collision lanes of the visually continuous spike bed.
Its independent original-ROM CPU trace confirms the tile IDs and samples.

An experimental repair preserves the existing point hitbox and detects
crossing the full opaque last-pixel base of the two continuous pit metatiles.
It does not make decorative fill solid. Its exact geometry has passed
739,840 rational-oracle comparisons, including both Y directions and room
boundaries. It stops the Theory witness at 1,429 with unchanged approach in
both emulators, and preserves the retained early-level hidden coin inputs.
However, it adds frame-budget pressure and changes many old clearing tapes.
It remains experimental and is being searched across all 109 levels.
No pit-engine proposal is represented here as ready to merge.

The ordinary upward spike in Every Madness (C-Sides 6) also tunnels:
frame 3,075 samples `(6,3)` and 3,076 samples `(9,7)`, skipping the lethal
x 4–8, y 4–11 rectangle. Hell (C-Sides 8) tunnels through a ceiling spike:
frame 1,084 samples `(4,1)` and 1,085 samples `(8,5)`, skipping the lethal
x 5–7, y 0–5 rectangle. Both new ordinary Dissonance witnesses have original
CPU-read traces. The broader experimental sweep stops them at 3,076 and
1,085 with unchanged approaches in QuickNES and independent FCEUmm.

`experimental-release-collision.patch` retains the broader prototype,
including the arithmetic repair and pit-base guard. It preserves all
original point kernels and adds exact segment tests for every spike kernel,
including unions of two rectangles. Teleports, mode/size transitions and
new dual-player positions invalidate history. The actual proposed C code
matches the original C point predicates at all 16,384 collision-class/pixel
combinations. Its 16 distinct shape unions pass 692,224 independent exact
rational segment-intersection checks. All four release variants build.
This is an engine-only experimental source patch: the combined search build
also uses the separate Fofii tile repair and corrected D continuation cache.
Timing and complete-level review are outstanding. Do not treat it as a
validated production patch.

## Reproducible random setups and detector correction

Changing only the Dissonance seed leaves a fixed genesis game RNG. Lucky
Draw contains 800 deliberate chance-of-death triggers, so repeatedly
searching one fixed startup timing provides poor coverage. The runner now
records ordinary menu waits and startup waits. New random-mode runs use a
180-frame cold startup in both cores. The old release QuickNES witnesses use
22 frames while their FCEUmm boot profile uses 180; those legacy profiles do
not establish RNG equality for random-mode levels.

FCEUmm now restores a serialized cold power-on state before each replay.
`cold-setup-comparison.json` verifies identical RNG bytes and active gameplay
in QuickNES and FCEUmm after menu waits 0, 1 and 3, then repeats wait 0 to
check that a previous replay cannot change the result. Only entry frame 1
differs. `luckydraw-cold-setup-proof.json.gz` is a Dissonance setup regression,
not a failing bug witness; its independent replay matches after entry.

The Bloodbath investigation exposed a release-engine coordinate bug. At frame
3,369, relative Y 253 plus centre offset 7 wraps to 4 before the camera is
added. The original ROM samples noncolliding tile 111 at Y 618; the player's
physical centre at Y 874 lies inside a solid block. The earlier classification
as only a detector error was incomplete: predicting the observed probe
correctly must not hide the physical collision miss. Detector revision 7
records these blind spots separately from false lines across coordinate jumps.

`release-centre.patch` preserves the carry and normalizes both 240-pixel room
boundaries. The first repair attempt still returned noncanonical packed
`01F0` for camera 225, relative Y 247, offset 8, instead of `0200`. That
attempt was rejected by executing its assembled instructions against a
linear-coordinate oracle. The final combined build passes 1,474,560 cases
covering every camera and relative Y, actual hitbox heights 7, 8 and 15,
and both mini flags. `check-centre-carry.py` runs that check directly on a
built ROM and its matching DBG file.

The isolated repair's C-side ROM is
`b15566ab63402b82495afb794bf82c3186ad6428372ee21de5bcefec81f2d553`.
It kills the original Dissonance witness at frame 3,368 with an unchanged
approach in QuickNES and FCEUmm (FCEUmm entry frame 4 differs). A fresh
ordinary Dissonance control clears it in 6,861 frames, with matching FCEUmm
gameplay. Its input and replay reports are retained here. Current upstream
already uses linear coordinates for these probes; this is a release repair,
not an outstanding current-upstream arithmetic defect. Other release body
probe calculations still need assessment.

The latest combined prototype is retained separately as
`experimental-release-collision-carry.patch`. It also skips empty spike
shapes and absent second primitives, and uses unsigned slab intersections.
The original experimental patch remains available for its earlier evidence.
The combined patch excludes the separately reviewed Fofii level edits and
corrected D-side continuation assets. All four combined ROMs build, and
every decoded tile and all 256 compiled collision classes match the combined
source index. `experimental-carry-builds.json` records all ROM hashes. The
collision bank uses all 8,192 bytes, so this build requires the pinned compiler;
there is no spare space in that bank. Full searches and
playability review remain outstanding; neither prototype is approved for
production. Older detector inventories and superseded build searches are
exploratory and do not count as clean rounds.

## Stale split-level compression cache

The 28-byte difference between the checked-in release D-Sides assets and the
official ROM consists entirely of three embedded continuation IDs. Windy
Landscape contains 0 instead of 1; Aftercatabath contains 0/1 instead of 2/3.
After decompression each difference follows the `127,127` continuation
marker. Freshly recompressing those chunks and retaining the official bank
placement reproduces the official D-Sides ROM byte for byte:
`b365667f0bb28a3397b46d648057ee71031050de41bf126959bd66ee03af8bf3`.
The source geometry itself was not the difference.

The same missing cache context remains in current upstream.
`current-export-cache.patch` adds the first continuation-table index to the
split-cache key. Changing earlier levels or export order invalidates the
cached embedded IDs; unchanged context still reuses them. Legacy caches are
rebuilt once. Its regression test drives the real exporter cache path with
stubbed compression: the original exporter fails after reordering two
split levels, and the patched exporter passes reordering, stable reuse and
legacy-cache invalidation. Dependencies are the existing exporter packages.

```sh
PYTHONPATH=LEVELS python3 -m unittest discover -s LEVELS -p test_split_cache.py
```

## Replay

Release campaign witnesses use `dissonance-replay.mjs` and `fceumm.py` from
the parent audit directory. Current witnesses additionally require the
matching build's DBG symbols for QuickNES; FCEUmm reads their recorded RAM
layout. Always use the recorded QuickNES core hash and the matching ROM.
Append `--fixed` only for the reviewed repair; it requires death at the
reported escape, before any later original death, with unchanged approach.
Patch application, builds, independent replays, coin objectives and NES
frame-budget checks remain separate requirements. No upstream submission
has been made. `sha256.json` records retained artifact identities.

## Current-upstream Fofii data repair

At `541d68ffcae2096f116146ef21cbc4d62886eb6b`, the original B-side ROM
`982e220d348379a24ae387a94dbc1aed51d5359f34f24993b2f240dc370a47b6`
still permits the three-cell floor escape. All 32,430 decoded Fofii background
tiles match the shared HUGE source. Fofii moved from release index 6 to
current index 10; the symbol adapter verifies the named ROM pointer and uses
release target 6 for Dissonance's length/progress model. An initial run with
index 6 selected A Jolly Retro Christmas and is invalid as Fofii evidence.

`current-fofii-witness.json.gz` is fresh ordinary input found by Dissonance.
Original replay survives below the level through frame 1,536, x 4,519, y 897;
FCEUmm matches the complete trace, including the entry animation. Applying
`../../examples/fofii-current.patch`, exporting B, and building with cc65
`6bd01e8` gives ROM
`23a9b4ea178f66db4c15f58c9df1f7376921ce9522b588e5e45e988a969fb3a4`.
Both cores match the original approach and kill that input at frame 1,317.
A fresh Dissonance control clears the repaired ROM in 3,764 frames with coins
1 and 3; FCEUmm matches every frame. The corresponding input, geometry and
replay reports are retained here. Both B and HUGE export and build successfully; `current-fofii-builds.json`
records their ROM identities and sizes. These are B-side replay proofs; HUGE
replay and broader objective coverage remain separate checks.


The next discovered release root is signed vertical integration. In Sonic Wave
(C-Sides 19), `common_gravity_routine` adds velocity -1594 to a position just above
zero on frame 2,359. The 16-bit result wraps to screen Y 255, and play continues
below the course. `c19-movement-wrap-witness.json.gz` retains the ordinary
Dissonance input; the collision and write traces identify the actual store.
`experimental-release-motion-isolated.patch` applies on top of the earlier
`experimental-release-collision.patch` combined search source, including its
separate data repairs. It changes only the common integration path and its fault
handling. Both retained independent-core replay results die at frame 2,359 with
an unchanged approach (FCEUmm differs only at intro frame 3).

The broader checked-motion prototype covers addition, subtraction, and signed
collision ejection. `check-vertical-motion.py` executes its assembled helpers
against unbounded arithmetic; 1,245,640 cases pass. It also preserves the
intentional wrap/debug/invincibility exemptions. This remains experimental:
fresh clearing runs and the complete candidate inventory are still required.
The combined prototype changes lag before the old Sonic Wave input's escape;
that input is only a strict repair proof for the isolated patch.

Nightmare's retained contextual CPU trace proves another release coordinate
error: fractional player/camera carry is lost before collision sampling. Current
upstream already includes that carry. `check-centre-carry.py --fractional` checks
both carry classes across all camera, screen-Y, supported hitbox-height and mini
combinations: 2,949,120 assembled cases pass on the checked-motion prototype.

The original combined search completed all 109 levels with 11,850,000 executions
and 83,342 candidates. Its later carry-corrected successor completed all 109
with 11,865,000 executions and 82,182 candidates. Both remain exploratory because
they lack the newly discovered signed-integration repair. No clean round has
been counted.

The three remaining solid-overlap entries in the broad combined search are
intentional wave D-block contacts. The retained `b-19-558` and `b-5-489/638`
witnesses and contextual FCEUmm collision traces all read the solid tile with
`dblocked == 1`; the engine deliberately permits that contact.
`wave-solid-dispositions.json` records these decisions. No wall patch is needed.


A fresh ordinary Dissonance clear on the broader checked-motion prototype is
retained as `c19-motion-fixed-control.json.gz`: 7,295 frames, reaching the genuine
level-complete state. FCEUmm matches the entire gameplay trace, with only intro
frame 3 differing. This is a positive control for that prototype.

The next confirmed surviving barrier miss is Sonar (B-Sides 0). Its background
spike-bed tile, metatile 12, shares `COL_DEATH_BOTTOM` but was excluded from the
pit-base guard. CPU probes at frames 1,799–1,802 pass from local `(6,8)` through
`(9,11)` to the row below; they avoid the narrow original centre kernel. The
ship continues below the strip. `b0-background-spike-witness.json.gz` and its
collision trace preserve the input. A prototype assigning this tile to the
existing `COL_PIT` family, preserving its ordinary point hitbox, dies at frame
1,802 in both QuickNES and FCEUmm with an unchanged approach. The retained
FCEUmm result differs only at intro frame 5. This family extension still needs
fresh clearing controls and a systematic review of the other spike-bed assets.
Consequently the checked-motion all-level run is exploratory as well: the
three-clean-round goal remains at zero.


The next prototype, `experimental-release-base-metadata.patch`, retains the
original collision classes and uses a separate packed four-bit base mask for
shared hazard tiles. A 128-byte table declares completed crossings of top,
bottom, left and right bases. The engine caches the actual metatile with each
collision sample; ordinary point kernels and narrow swept kernels remain the
same. This supersedes the collision-alias prototype as the approach under test.
The engine patch excludes the separate Fofii edits and D-side continuation
assets needed for the combined search build.

Forty tile definitions have 43 base edges verified across all 24 graphics
variants (1,032 checks). Animated saw edges and four changing diagonal edges
were excluded by that check. Three fully filled cloud tiles remain unannotated
because their forgiving hitboxes do not define a distinct structural base.
The C base geometry passes 1,583,104 exact cases, and the independent review
detector passes the same reference on 1,557,504 crossings. Original point and
narrow swept geometry pass 16,384 and 692,224 cases respectively. Every decoded
level tile, compiled collision class, and packed base byte matches the source
index for all 109 levels. Build identities are in
`experimental-metadata-builds.json`. Playability and full searches remain
required; this is not a production-approved patch.

Fresh controls for the earlier Sonar-only base extension and the current
upstream checked-motion Theory of Everything prototype are retained. FCEUmm
matches the complete gameplay of both: Sonar clears in 5,090 frames (intro frame
5 differs); current Theory of Everything clears in 4,971 (intro frame 3 differs).
These controls apply to their recorded ROMs, not later prototype hashes.

The completed carry-prototype inventory contains five surviving narrow spike
contacts, all with a one-pixel fractional-camera discrepancy in CPU probes.
Three retained inputs—B-Sides 18, main 18, and main 40—now die on their reviewed
frames under the metadata/fraction repair, with unchanged approaches in both
cores. Their contextual collision traces and replay results are retained here.
The original official ROMs independently reproduce all three contacts; the
`*-official-*` witnesses record those identities. B-Sides 30 also has a real
earlier collision at frame 596: the original centre query is one pixel above
the physical centre and misses a bottom half slab. Both cores confirm the
repair at that frame with the approach unchanged.

C-Sides 21 candidate 412 is withdrawn as an upstream finding. The official
ROM already kills that input at frame 454. An earlier prototype changed pit
collision classes without updating every body-collision handler, making the
later route reachable only in that prototype. The metadata build restores
all 256 original collision bytes in every ROM; the byte-for-byte comparison
and original-input negative control are retained. No clean round has been
counted.


The isolated original-release integration proof now includes C-Sides 18 Eon.
The official ROM wraps vertically at frame 2,777 after a signed velocity
addition. The minimal checked-add repair kills at the same frame with an
unchanged approach in QuickNES and FCEUmm. An official C-Sides 19 witness
also demonstrates the wrap, but its input diverges earlier under the isolated
repair; it is evidence of the original failure, not a strict paired fix proof.

A search on the metadata prototype found a surviving right-base crossing in
Theory of Everything 2 (main 17), at frame 4,940. A redundant wave portal
cleared history while the player was already a mini wave. CPU RAM writes
confirm that the history invalidation skipped the crossing test. The isolated
conditional-wave reset repair stops that input at the crossing in both cores.
This is a gap in the proposed history guard, not a separate portal bug in the
released engine, which has no such guard.

`experimental-release-history.patch` generalizes the repair by recording mode
and size with each player's collision sample. Actual shape changes invalidate
comparison centrally; redundant portals retain it. Dual and single portals
invalidate only a player whose position is spawned or resynchronized, and
teleports invalidate only actual displacement. The latest combined build
preserves all 109 levels' indexed geometry, original collision classes and
base metadata. Both cores kill the TOE2 witness at frame 4,940 and confirm an
actual reset 40 released frames later. Fixed campaign replay checks now require
committed death followed by a reset, rather than accepting a transient flag.
The next complete Dissonance search is running on these frozen ROMs. The
three-consecutive-clean-round count remains zero.


The history guard has also been ported to upstream `541d68f` as
`experimental-current-history.patch`. All four standard ROM sets build at the
original 786,448-byte size. A dedicated collision segment shares the existing
sound-effects bank after its unchanged audio data. The selected shared levels'
ROM geometry and metadata match their index, and the exact point, sweep and
base oracles pass. Current Fofii and Theory of Everything failing inputs die
at frames 1,317 and 1,428 respectively in both cores, with unchanged gameplay
approaches and confirmed level resets. This is a source proposal under broader
validation, not a submitted upstream change.

The next source review found a distinct level boundary gap in Theory of
Everything. A UFO can pass around a stair corner formed by adjacent pit tiles
and enter the closed `BLACK_NONE` component at columns 243–270, rows 55–56.
It crosses neither a narrow hazard kernel nor a declared opaque base, so the
continuous collision repair alone cannot close it. The official release input
survives to frame 1,567 below the course; both cores agree. Current upstream
also reproduces it. The component contains no sprite; coin 1 is at column 263,
row 45, ten rows above. The CPU collision trace and source diagram are retained.

`toe-stair-release.patch` and `toe-stair-current.patch` propose one left-facing
spike at column 245, row 55. Both release and current upstream repairs kill the
input at frame 1,436 with unchanged gameplay approaches in QuickNES and FCEUmm,
then reset normally. A fresh repaired release control clears in 4,977 frames
and matches FCEUmm throughout gameplay. The current source is shared by A and
HUGE; both require regenerated exports. Coin-route coverage and the remaining
HUGE validation are still required before proposing the data change upstream.
The release exporter repacked data into a bank already occupied by music. The
4136-byte changed payload has exactly its original length, so this experimental
release build preserves the original generated level/sprite bank assignments;
all 109 decoded levels and bank capacities are verified after linking.

`boundary-corners.py` generalizes this source review without editing levels.
It inventories interior cells where equal-facing spike bases form a stair
corner, including each connected fill component and its sprites. There are
325 locations in 24 installed levels, not 325 confirmed bugs. For example,
similar Deadlocked cells are in a component containing a teleport exit and
pads; blocking that component without reviewing its route would be wrong.
The inventory is a way to select ordinary-input reproductions and explicitly
review intent. It is not an instruction to turn decorative fill solid.
The all-level history build sweep continues as an exploratory inventory and
is disqualified from the clean-round count by the confirmed stair escape.

Stereo Madness also has a confirmed original-ROM collision bug. The retained
late-spike input crosses the corner of a spike's existing narrow kernel between
frames 4,018 and 4,019, well after the hidden-coin routes. FCEUmm matches the
original 4,661-frame trace. The continuous collision repair kills at frame 4,019
with the approach unchanged in both cores and confirms a normal reset.
This finding does not reinstate the withdrawn early-level tile patches.

Geometrical Dominator has a separate local boundary gap below the small saw at
column 916, row 23. A mini UFO avoids the saw's narrow kernel and enters a closed
fill component at columns 916–922, rows 24–26, containing no objective or teleport
exit. `geodom-saw-release.patch` and `geodom-saw-current.patch` place one upward
spike at column 916, row 24. The release input survives 4,942 frames; both cores
agree. The repaired release kills at frame 4,901 with its approach unchanged,
and a 5,788-frame clearing control remains mechanically identical.

The current-upstream undersaw witness independently reproduces that same gap.
A clean upstream build with only this tile edit kills at frame 4,901, with the
approach unchanged in both cores and a confirmed reset. The original current
trace survives 4,914 frames. This data-only proof is distinct from the older
current `geodom-saw` input, which actually entered through a neighboring pit
base and could be repaired by the generic base guard. General coin-route
coverage remains pending before upstream submission.

The finish trigger has a generic viewport bug. In Hi, the end sprite is at
world Y 544 while the lower route puts the camera at Y 719. The sprite enters
the horizontal window at frame 6,837 but remains inactive because of vertical
culling. An original-ROM input survives another 1,200 released frames beyond
the course; QuickNES and FCEUmm match all 8,145 frames.

`finish-camera-release.patch` and `finish-camera-current.patch` preserve the
horizontal gate but activate end triggers regardless of camera height. The
compact gate also preserves the three animated-coin types, verified against
12,288 cases executed from the assembled ROM. On a combined repair prototype,
all three cores (QuickNES, FCEUmm, Mesen) complete at frame 6,843 with the
approach unchanged. On an isolated original-release repair, QuickNES completes
at frame 6,839 with an unchanged approach; FCEUmm incurs an earlier lag at
frame 1,248 and that input dies at 1,323. That isolated attempt is retained as
a rejected independent control, not as a strict paired fix proof.

Across 193 retained release inputs, 191 are mechanically unchanged by the
compact finish gate. Main 24 still clears with timing differences. The old
Windy Landscape clearing input dies; a fresh Dissonance input clears in both
cores at frame 5,047, with 48 differing mechanical frames. Both that input and
the actual independent trace are retained. This resolves the clearing-control
gap without claiming an exact trace match.

`experimental-current-huge-layout.patch` applies after the current history
patch. It puts the new collision kernels in the existing collision/sound bank
for HUGE and keeps rare arithmetic error handlers there behind checked bank
dispatches. Standard builds retain their layout. All five current standard sets and
HUGE link. All 173 HUGE levels (5,640,443 tiles), collision classes and base
metadata match source. The assembled arithmetic oracle passes 1,245,640 cases,
including 155,002 checked dispatch arguments; it does not model the mapper.
The repaired HUGE Fofii control clears in Mesen at frame 3,764 with coin mask
5. Its mechanics match the standard control after entry frame 1, excluding
the changed level ID (59 in HUGE). This is an ordinary-input control, not a
173-level Dissonance search.

The current experimental engine patch sequence is the current history patch,
HUGE layout patch, then current finish patch. The Fofii, TOE stair and Geodom
TMX changes are separate data proposals requiring exports for A/B/HUGE as
applicable. This package remains under validation. The completed exploratory round 38 searched all 109 installed levels with
12,630,000 executions. All 82,285 candidates replay without endpoint mismatch.
Source dispositions remain incomplete and newly confirmed bugs disqualify
that round. The three-consecutive-clean-round count is zero. No production ROM is replaced.

Kappaclysm has a stepped-floor gap in component 135, columns 242–268, rows
24–26. It contains no objective or teleport exit. The initial single cap at
(244,25) blocks the release witness but leaves two current-upstream witnesses
unchanged: they enter at (247,24) and (242,26). The `kappa-stair-v2` proposals
replace all six exposed corner cells of this reviewed component. Version 1
files remain as evidence of the insufficient first repair.

The release data-only repair responds at frame 1,542 and kills at 1,544.
Solid contact legitimately changes the pose before death; this is not an
unchanged-until-death proof. The 4,840-frame original clearing input remains
exact. A fresh 4,754-frame Dissonance clearing input also remains exact under
the combined v2 repair. Current-upstream upper and lower witnesses respond
at 1,524/1,447 and die at 1,525/1,449. All three repaired traces match FCEUmm
and reset normally. A fresh current combined-repair input clears at 4,841
with coin mask 4 and matches FCEUmm after entry frame 1.

Silent Clubstep (release D4, current E21) has a different rounded-cap gap.
The historical Dissonance witness, formerly misnamed `kratos`, avoids the
small saw's narrow kernel at (29,52) and enters closed component 130 at
(29,53). This component contains no sprites or open empty-cell faces. One
released-input frame was removed at frame 166 to reproduce the same route
on the original ROM; the retiming attempts and independently matched
1,127-frame original trace are retained explicitly.

The `silent-rounded-cap` proposals replace that half-saw tile with PITSPIKES
to declare an opaque bottom base. They require the shared base guard: this
is not a tile-only fix for the original point-sampling engine. The combined
repair kills the historical input at frame 198 with its approach unchanged
in both cores and confirms reset. A whole-level clearing control and an
ordinary current-upstream failure proof remain pending, so this cap proposal
is experimental.

The complete current combined source builds all five standard sets and HUGE.
Compiled geometry and collision metadata match all 170 standard and 173 HUGE
levels. The HUGE Fofii input clears at frame 3,764 in Mesen with coin mask 5.
The retained patch-series check reconstructs the source changes exactly;
geometry and build coverage do not imply behavioral coverage of every level.

The schema-3 boundary inventory includes 354 stair corners and 11,426 rounded
hazard/fill adjacencies across 74 levels. Source-edge stair corners are now
included, which exposes Kappa's previously missed (242,26) corner. Components
record sprites by origin tile. These inventories identify review candidates;
they neither classify bugs nor apply automatic geometry edits.

The sole round-38 base-crossing signal in Endorphin Rush (C5, frame 2,365)
is a detector false positive. Read-only history tracing verifies all 2,426
witness frames: the actual collision point moves from Y 642 to 641, while
the post-frame estimator predicts 641 to 640. The opaque top slab spans
640–641, so the actual point never crosses its full thickness. FCEUmm
diverges earlier; this phase diagnosis is an exact QuickNES witness replay,
not an independently matched FCEUmm contact trace. The disposition and
hashed history output are retained.

Deadlocked exposes a shared wave/slope failure in both the release and current
upstream. The normal wave is exempted from LU45 slope tests, enters the
opaque upper-left triangle at frame 891 and continues below the course.
Original ordinary-input witnesses match FCEUmm; current discovery came from
a separate actual Dissonance search.

The aligned-wave slope patches are experimental additions to the combined
reviewed-cap prototypes, not standalone diffs against pristine upstream.
They preserve the original body slope routine and its mini-wave exclusions.
A separate normal-wave point test samples LU45 four pixels right of the
existing death probe, aligning it with the wave renderer and body offset.
The helper resamples at tile seams and restores the previous probe metadata.
The release helper uses XCD_BANK_04; current uses COLLISION_BANK and also
selects normal Snake, whose dimensions and renderer use the same alignment.
Current Snake runtime coverage remains pending.

Both combined repairs kill their original Deadlocked witness at frame 891
with every approach frame unchanged in QuickNES and FCEUmm, followed by a
confirmed reset. A fresh Dissonance input clears the repaired release at
frame 5,921; all frames match FCEUmm. The host oracle compiles the actual
helper and checks 3,538,944 byte-coordinate, seam, player and incoming-state
cases, including preservation of the previous probe. It does not validate
mapper operation or establish sprite alignment by itself.

The complete paired comparison preserves 168 of 201 inputs exactly. Of 125
previously clearing inputs, 97 still clear. Seventeen lost controls first
differ at a new death; read-only replay inspection finds D-block protection
off at every such contact. The remaining eleven first differ mechanically
before death. These losses require further route and timing assessment; the
new clear does not establish general playability. A rejected earlier raw
slope-context variant retained only 94 of 125 clears and changed mini-wave
behavior without a confirmed mini-wave failure proof. Its paired comparison
is retained as negative evidence. No slope variant is accepted for shipping.

Exploratory round 43 searches all 109 installed levels using the aligned
prototype and freshly validated ordinary prefixes from round 38. Its source
dispositions and affected clearing controls remain pending. Clean rounds: 0.

All three round-38 solid-block signals are intentional wave D-block contacts:
Moonlight (582,12), Moonlight (672,12), and DoraBae Basic 10 (524,23). Each
new representative now has an ordinary-input witness and independently
matched FCEUmm CPU collision trace. All centre samples in these windows have
D-block protection enabled. Their explicit dispositions do not suppress
future solid-block signals elsewhere.

Forest Temple has a separately reproduced quarter-block boundary gap
(release D2, current E13). Original release input matches all 3,728 frames
in FCEUmm. Its centre passes through the empty left and lower portions of
SMALL_BLOCK_TOP_RIGHT at (468,25), then enters BLACK_NONE at (468,26),
frame 2,605. Enclosed component 54 spans columns 450–630, rows 23–26 and
has no sprites or empty-cell open faces. It rides beneath the course.

Fresh current-upstream Dissonance discovery reproduces the same below-floor
route. The current CPU centre reaches BLACK_NONE at frame 3,054. Its extended
3,573-frame ordinary input remains below the floor and matches FCEUmm after
entry frame 3. The current level geometry and collision classes match source.
No current engine repair is needed to reproduce the issue.

The `forest-quarter-cap` proposals are diagnostic one-cell repairs: BLACK_NONE
to BLACK_TOP at (468,26). On isolated source-data builds, release contact
changes the pose at 2,604 and death follows at 2,605; current contact starts
at 3,052 and death follows at 3,058. Actual repaired traces match FCEUmm.
These are contact-before-death comparisons, not unchanged-until-death claims.
The release 5,399-frame and current 5,811-frame clearing inputs remain exactly
unchanged, including current coin mask 2, and match FCEUmm after entry.

A preliminary SPIKE_UP cap failed to block the release input because it
avoided that tile's narrow spike kernel; it is not the retained proposal.
The solid one-cell cap is not claimed to repair the entire underfloor
perimeter. Six quarter-block/fill adjacencies belong to this same component,
and additional entry signals still require first-ingress tracing.

Schema-4 inventory adds 767 quarter-block/fill adjacencies in the round-38
source, with component references. That source has 353 stair candidates and
11,424 rounded hazard adjacencies after its earlier cap edits. All counts
are review leads, not confirmed bugs or automatic tile edits. Exploratory
round 43 lacks the new Forest Temple cap and cannot qualify as a clean round.

The LU45-only candidate is incomplete. A separate Forest Temple wave route
passes through a diagonal RD45 boundary into enclosed component 42. Its
2,289-frame trace is identical on the original release and LU45 candidate.
FCEUmm matches the original trace. The old probe moves from EMPTY at
(x low 111, Y 877) to BLACK_NONE at (115,880); the aligned point is already
inside the adjacent RD45 triangle on the preceding frame.

`wave-four-slopes` generalizes the experimental aligned point to all four
45-degree orientations, preserving the original body routine and mini-wave
behavior. The release helper uses XCD_BANK_02; current uses COLLISION_BANK.
All four release and all five current standard builds plus HUGE link. The
release compiled geometry and collision metadata match all 109 installed
levels. The host oracle checks 14,155,776 room-local cases and 53,084,160
current linear-coordinate cases, including metadata restoration at seams.
It does not establish mapper correctness or cover the 22/66-degree slopes.

Both cores block the release Forest Temple wave input at frame 1,774, with
its entire approach unchanged, and confirm reset. A separate fresh current
Dissonance witness is blocked at 1,848 with its approach unchanged in both
cores, followed by reset. The current old probe reaches BLACK_NONE at 1,850;
its earlier coarse fill signal at 1,860 was not the first entry. Both
Deadlocked witnesses still die at frame 891 with unchanged approaches in
both cores. The 5,921-frame fresh Deadlocked clear remains exactly unchanged.

Of 201 paired release inputs, 160 traces remain exact; 95 of 125 previously
clearing inputs still clear. Affected routes and timing remain under review.
A current Forest Temple clearing input dies at 1,719 in mini-ship mode on
the combined repairs before any wave-point change, identically on LU45 and
four-orientation prototypes. This is not a wave regression. Fresh current
combined-repair clearing discovery is running; this patch is experimental.

Control intent review rejects the fresh 5,123-frame release Forest Temple
clear: it still uses the separate quarter-block hole. Its verified probe
history contains 821 samples in the forbidden underfloor region, with no
source/recorded-metatile mismatch. It is retained as a reviewed quarter-gap
witness, not a whole-level playability control.

A fresh current combined-repair Dissonance input clears in 5,234 frames with
coin mask 3 and matches FCEUmm after entry frame 3. Every witness frame is
verified again in read-only history tracing. The biased and aligned wave
probe positions avoid both selected enclosed regions, and every recorded
metatile matches source. This establishes avoidance of those two known
escapes; it does not classify every other course boundary.

The next combined prototype includes solid caps below all six quarter-block
adjacencies of the reviewed Forest Temple component (columns 468, 470, 475,
480, 502, 504 at row 26). The current clearing input remains exact. This is
under validation, including a new release clearing search; rounded saw
adjacencies elsewhere along the same boundary still require input review.
The 109-level round 43 continues on the older LU45 prototype to finish
coverage, but known Forest Temple bugs disqualify it. Clean rounds remain 0.
