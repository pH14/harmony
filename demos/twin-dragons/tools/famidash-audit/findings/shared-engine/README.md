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
