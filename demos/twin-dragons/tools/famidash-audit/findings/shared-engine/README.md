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
