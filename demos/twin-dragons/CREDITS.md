# Credits and licenses

## Twin Dragons

**Twin Dragons** is by **Garydos**. This Harmony demonstration is not endorsed
by the authors.

- Source: https://github.com/Garydos/twin-dragons-nes at commit
  `bcf27d0c5117a2aac19e6c33bdb3c776ada069fb`
- Game code: **zlib license**. The license text ships as
  `licenses/twin-dragons-LICENSE.txt`.
- This build is altered. The sound-effect data in the released ROM is zeroed;
  every other ROM byte is unchanged. `workloads/nes/scripts/build-twin-dragons-rom.sh`
  in the Harmony repository makes the change.
- Some artwork by **surt**, https://opengameart.org/content/twin-dragons,
  **CC BY 3.0** (https://creativecommons.org/licenses/by/3.0/). Modified: the
  level maps and the player sprite sheet are rendered from the game's tiles and
  palette and composed into images. Browser maps add heat overlays and player
  sprites.
- Music by **Garydos**, **CC BY 4.0** (https://creativecommons.org/licenses/by/4.0/).
- **FamiTone2** by Shiru and **FamiTone5** by nesdoug are public domain.
- Sound effects by Shiru are not included.

Level maps are drawn from the game's level, metatile, tile and palette data at
the pinned commit. The player sprite sheet is drawn from the game's sprite
tiles, frame layouts and palette. The build checks both against emulator
frames. Exported images and histories carry these credits in their metadata.

The deployed site ships the altered ROM, the upstream zlib license, these
credits, the pinned QuickNES source, and the browser frontend and build recipe
in `licenses/`. `harmony-source.tar.gz` holds Harmony's complete source.
`rust-dependencies.tar.gz` holds the locked Rust dependency sources and their
license notices, including miniz_oxide (MIT, Zlib or Apache-2.0) and adler2
(0BSD, MIT or Apache-2.0).

## Famidash

**Famidash** is by **TFDSoft**, copyright 2023-2024 TFDSoft. It is an
unofficial fan demake of **Geometry Dash** by RobTop Games and is not
affiliated with or endorsed by RobTop Games. This demonstration is not
endorsed by the authors.

- Release: https://github.com/tfdsoft/famidash/releases/tag/1.2.8, built from
  commit `0df680d0e02f0ccb438f1224d162a0ab073b7204`
- Game code: **MIT license**, shipped as `licenses/famidash-LICENSE.txt`.
- The site ships the official release ROM `Famidash.v1.2.8.nes` unchanged.
  `workloads/nes/scripts/build-famidash-rom.sh` downloads it and checks its
  SHA-256.
- Music is arranged from tracks by ForeverBound, DJVI, Step, Waterflame,
  Dj-Nate and OcularNebula. The upstream music credits ship as
  `licenses/famidash-CREDITS.md`.
- Modified: the level maps and the player sprite sheet are captured from the
  game's screens and composed into images. Browser maps add heat overlays and
  player sprites.

## Emulators

**QuickNES** by Shay Green and contributors, with the libretro port by its
contributors, is pinned to `26bb785c9deddb66a17717b21bb4e328f03ade32`:
https://github.com/libretro/QuickNES_Core/tree/26bb785c9deddb66a17717b21bb4e328f03ade32
The `nes_emu` library sources use **LGPL-2.1-or-later**. The libretro
distribution ships **GPL-2.0** terms, which this demo applies to the compiled
QuickNES browser core. Its C++ shim (`tools/frontend.cpp`) is
**GPL-2.0-or-later**. See the shipped `QuickNES.txt` and source-file notices.
The browser core applies Harmony's idle-loop patch
(`quicknes-idle-loop.patch`), which changes only CPU speed, and its
four-screen state patch (`quicknes-four-screen-state.patch`), which lets the
core restore saved states of four-screen cartridges such as Famidash.
The compiled core is distributed with its corresponding source and build recipe.

**SameBoy** by Lior Halphon runs Tobu Tobu Girl Deluxe. It is pinned to
`98da08c3117a7bf39c7329fc662c0d18cb54707d` (v1.0.4):
https://github.com/LIJI32/SameBoy/tree/98da08c3117a7bf39c7329fc662c0d18cb54707d
SameBoy is under the **Expat license**, shipped as `sameboy-LICENSE.txt`. The
browser core applies a patch that skips idle CPU time during HALT. The patch
changes only emulator speed. It is part of `build-sameboy-runtime.sh`, which
ships with the frontend `sameboy-frontend.c` and `sameboy-source.tar.gz`.
The core runs SameBoy's own boot ROM, built from the same source.

## Tobu Tobu Girl Deluxe

**Tobu Tobu Girl Deluxe** is by **Tangram Games**. This demonstration is not endorsed by the authors.

- Source: https://github.com/SimonLarsen/tobutobugirl-dx at commit
  `4985d4d282aa43229f250ffa3b0d86998ebd55d3`
- Game code: **MIT license**, shipped as `licenses/tobu-tobu-girl-LICENSE.txt`.
- Images, text, sound and music: **CC BY 4.0**
  (https://creativecommons.org/licenses/by/4.0/).
- The ROM is compiled from that source with a pinned toolchain by
  `workloads/gb/scripts/build-tobu-tobu-girl-rom.sh`.
- Modified: the level maps and the player sprite sheet are captured from the
  game's screens and composed into images. Each level background is stretched
  to the level's height. Browser maps add heat overlays and player sprites.

## Super Mario Bros.

Super Mario Bros. is © Nintendo. The site ships no part of the game. A visitor
supplies the ROM, which stays in that browser's storage. The browser draws the
level maps and the player sprite from that ROM. This demonstration is not
endorsed by Nintendo.

## Harmony

The browser interface and Harmony/Dissonance Rust code are
**AGPL-3.0-or-later**: https://github.com/pH14/harmony

No game assets are relicensed as Harmony code. No advertising or payments are
part of this demonstration.
