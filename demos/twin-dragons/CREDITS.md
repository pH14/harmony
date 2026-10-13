# Twin Dragons — credits and licenses

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

**QuickNES** by Shay Green and contributors, with the libretro port by its
contributors, is pinned to `26bb785c9deddb66a17717b21bb4e328f03ade32`:
https://github.com/libretro/QuickNES_Core/tree/26bb785c9deddb66a17717b21bb4e328f03ade32
The `nes_emu` library sources use **LGPL-2.1-or-later**. The libretro
distribution ships **GPL-2.0** terms, which this demo applies to the compiled
QuickNES browser core. Its C++ shim (`tools/frontend.cpp`) is
**GPL-2.0-or-later**. See the shipped `QuickNES.txt` and source-file notices.
The browser core applies Harmony's idle-loop patch
(`quicknes-idle-loop.patch`), which changes only CPU speed.
The compiled core is distributed with its corresponding source and build recipe.

The browser interface and Harmony/Dissonance Rust code are
**AGPL-3.0-or-later**: https://github.com/pH14/harmony

No game assets are relicensed as Harmony code. No advertising or payments are
part of this demonstration.
