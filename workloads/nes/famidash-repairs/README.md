<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Harmony Famidash repair build

Harmony ships `harmony-1`, derived from the official Famidash 1.2.8 release at
`0df680d0e02f0ccb438f1224d162a0ab073b7204`. The four ROMs contain shared
collision repairs and nine narrowly scoped level-data repairs. They run in the
normal QuickNES core; the visualizer does not filter escaped states or change
gameplay. Upstream Famidash code and data are MIT licensed by TFDSoft; see
[LICENSE-FAMIDASH.txt](LICENSE-FAMIDASH.txt).

## Build and provenance

`../scripts/build-famidash-rom.sh OUTPUT` downloads and checks the four official
inputs, applies `roms.json` through `apply.py`, verifies all four output digests,
and installs the results with the license, credits and music metadata. Delta
blocks are ordered, bounded replacements; they cannot select other files. All
inputs and outputs validate before installation begins. The browser and native
workload pins in `../famidash-versions.env` and `nes-games` select the repaired
ROMs. Browser map cache keys and campaign machine identities already include
the ROM digest, so original-ROM state is not reused as repaired-ROM state.

The delta avoids adding a cc65 compiler to every site build. It is generated
from a source build, with its complete source changes in `source.patch`,
per-file hashes in `source-files.json`, and individual tile edits in
`level-edits.json`. Generated compressed level payloads and continuation
metadata are included so the exact build does not depend on exporter package
versions or cache state.

To reproduce from source, use a disposable pristine checkout of Famidash at the
revision above. Build cc65 from pristine revision
`2f25b1807450db2d386f027b6d81dc906ab03396` (`make -j2` in its checkout), then run:

```sh
python3 workloads/nes/famidash-repairs/rebuild.py /path/to/famidash /path/to/cc65
```

This checks revisions and input files, applies the source patch, force-builds
main/B/C/D, and requires every ROM to match the shipped digest. It modifies the
provided Famidash checkout. A fresh-checkout rebuild has reproduced all four
outputs byte for byte. Standard main/B/C/D are the shipping configurations;
other upstream configurations are outside this build's qualification.

## Repairs

Shared changes preserve the original discrete collision checks and add bounded
movement checks for spikes, opaque bases, and wave slopes. Mini-wave checks
cover 22°, 45° and 66° slope orientations. History is separate per player and
invalidated on reset, mode/size changes and coordinate replacement. The 22°
and 45° kernels share the existing sweep's map reads. Additional changes retain
collision-coordinate carry, guard unsigned-Y ejection, correct ceiling-slope
contact selection, prevent spider penetration, and retain finish detection.

The level changes are in Fofii, Theory of Everything, Blast Processing,
Geometrical Dominator, Kappaclysm, Dark Paradise, Silent Clubstep, Forest Temple
and XX. Sprite layers, including coins and portals, are unchanged. They do not
make every decorative fill tile globally solid. Theory of Everything replaces
the whole reviewed 54-cell underfloor region after narrower caps were bypassed.
The original hidden-coin routes that caused early proposals to be withdrawn
remain outside these level edits.

The 22° helper occupies spare space after the existing PCM segment. Audio
bytes, CPU addresses and physical ROM placement remain unchanged in all four
builds. The actual assembled query decision has been checked across 131,072
combinations per build; the 22° kernel against 74,850 exact geometry cases,
2,048 original point masks and 65,536 signed products. All 109 decoded levels
and all compiled collision tables match the repaired source.

## Regressions

From the repository root, after building the browser QuickNES runtime:

```sh
python3 -m unittest discover -s workloads/nes/famidash-repairs
npm --prefix demos/twin-dragons run test:famidash-audit
python3 workloads/nes/famidash-repairs/regressions.py /path/to/patched-roms /path/to/runtime
```

The replay suite checks 11 unchanged negative inputs, including mini-wave22,
mini-wave45, both sizes of steep wave, both normal-wave45 corners, Aftercatabath,
xStep, and three Kappaclysm cavities. It requires the original approach followed
by death at the reviewed crossing and a real reset. Fofii's original input dies
at frame 1,317 while its existing winning route still clears at 3,764.

Theory of Everything is a different collision response: its unchanged input
first contacts the new floor at frame 1,422, lands, then dies at 1,425. The
contact checker compares the complete recorded repaired trace, checks the
unchanged approach and normal reset, and does not pretend this is a death-only
comparison. Three full clearing traces cover Theory of Everything (4,976),
Fingerdash (5,069), and a fresh Dissonance Dorabae clear (4,061). Positive controls
may adjust duplicate-frame duration while preserving every mechanical
transition; negative inputs are never retimed.

The workflow runs these regressions against its built ROMs and QuickNES core.
The static maps are regenerated from the repaired ROMs. Capture routes may use
the map composer's existing explicit assistance; they are not gameplay or coin
controls. `build-famidash-maps.mjs --repair` regenerates invalid capture routes
and persists them; ordinary builds replay the checked-in routes without search.

## Limits

A 109-level smoke search on these exact ROMs completed 283,536 Dissonance
executions and retained 5,617 candidates. This is a bounded integration check,
not a claim that every route or objective has been assessed. Earlier complete
campaigns used different experimental ROMs and do not qualify this build.
The broader source assessment and three consecutive clean full campaigns
remain unfinished; clean-round count is zero. Retained audit findings describe
other candidate behavior and the qualification limits of each repair.
