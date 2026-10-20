<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Fofii pit escape in Famidash 1.2.8

This is a level-data bug in B-Sides level 6, Fofii Fofii Fofii. The pinned
release ROM reproduces it in both the arcade's QuickNES WebAssembly core and
FCEUmm. It does not require snapshot restoration, cheats, or gameplay RAM
writes. Setup selects level 6 by writing the level-select index, as the
arcade does; all subsequent input is A or no buttons.

`input.json` records 1,463 gameplay frames from the normal arcade level start.
The cube drops through the pit near x 3,740, reaches world y 897 by frame
1,340, and remains alive at (4,268, 897) on frame 1,463. Another 300 released
frames reach (5,301, 897), still alive. Debug mode and the invincibility counter
are zero at the escape. The 1,463-frame QuickNES and FCEUmm traces agree on
position, camera position, velocity, mode, gravity, size and death flags.

## Cause and fix

The TMX background layer has a three-tile opening at zero-based columns
233–235, row 38. Those tiles are `BLACK_NONE` (metatile 111), with `COL_NONE`.
The next row, 39, uses `X_NO_OUTLINE` (79), also `COL_NONE`. These world
coordinates are x 3,728–3,775 and y 768–799: the 47-row level is bottom-aligned
with an offset of 160 pixels. The collision maps in cartridge RAM match the
source tiles. There is an open path into the non-colliding decorative fill;
the cube eventually lands on the global ground below the intended course.
It is not tunnelling through a solid platform or wrapping its coordinates.

`upstream.patch` replaces the three row-39 fill tiles with `SPIKE_UP` (17;
TMX GID 18). This puts hazards at the bottom of the pit, preserving the jump
and orb route above it. The patch includes the regenerated compressed level,
its cache hashes and the generated size annotations. It changes no collision
engine code or global tile definitions. This fixes this pit; it does not
claim that every escape in other levels has the same cause.

On the patched ROM the reproducer dies at frame 1,317, (3,766, 785), in both
QuickNES and FCEUmm. The existing clearing route still completes on frame
3,764 in QuickNES. An additional 2,000 input variations from the clearing
route found no surviving below-course state in the tested x 3,300–5,000
region (a sampling check, not an exhaustive proof).

## Run the reproduction

From this directory, point the QuickNES runner at the B-Sides ROM and a built
arcade public directory containing `engine/quicknes.js` and `quicknes.wasm`:

```sh
node replay.mjs /path/to/famidash-b-sides.nes /path/to/arcade/public
node replay.mjs /path/to/famidash-b-sides-fixed.nes /path/to/arcade/public fixed
```

The original mode checks the release ROM SHA-256. Both modes also play the
checked-in winning route as a control. The runner only needs Node and the
QuickNES runtime; it does not require building Harmony's Rust crates.

For an independent emulator, build libretro FCEUmm and run:

```sh
python3 replay-fceumm.py /path/to/fceumm_libretro.so /path/to/famidash-b-sides.nes
python3 replay-fceumm.py /path/to/fceumm_libretro.so /path/to/famidash-b-sides-fixed.nes fixed
```

The independent check used FCEUmm commit
`ac0d00a7fc01166f84c7bac01d1084ebfd2b865e`. The FCEUmm
runner waits longer at the title screen; the level-start gameplay state and
the gameplay input sequence match QuickNES.

## Apply the fix upstream

Apply the patch in a Famidash checkout at release commit
`0df680d0e02f0ccb438f1224d162a0ab073b7204`, then force a B-Sides rebuild.
`-B` matters because the upstream makefile does not track every incbin
dependency.

```sh
git apply /path/to/upstream.patch
make -B b-sides
```

The compressed Fofii level grows from 5,434 to 5,440 bytes. Data bank 10
grows from 8,168 to 8,174 bytes, within its 8,192-byte capacity.

The arcade now ships the combined Harmony repair build, which includes this
pit repair. Its pinned checksum, reproducible source build and regression
checks are in `workloads/nes/famidash-repairs`. The original mode above remains
a historical reproduction using the official release ROM.

The upstream changes are derived from Famidash by TFDSoft, MIT licensed.
See the license in the upstream checkout and the arcade's Famidash credits.
