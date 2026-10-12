# Nova the Squirrel — credits and licenses

**Nova the Squirrel** is by **NovaSquirrel**. This is a noncommercial Harmony
project demonstration using the original game's graphics and levels, not an
independent use or redesign of the Nova character. It is not endorsed by the
creator.

- Play/support the original: https://novasquirrel.itch.io/nova-the-squirrel
- Source: https://github.com/NovaSquirrel/NovaTheSquirrel/tree/e9e79ae59b188348bd6a87117a2d5c86a34ba433
- Game code: **GPL-3.0-or-later**.
- Graphics, levels and other assets: **CC BY-NC-SA 4.0**:
  https://creativecommons.org/licenses/by-nc-sa/4.0/
- Upstream additionally states: “Permission is not granted to use the character
  and design of Nova in anything else.” Upstream also asks that cameo characters
  not be used elsewhere. These notices remain applicable.

The ROM is source-built from that pinned revision using Harmony's reproducible
recipe. The displayed project-day build stamp is fixed to `1788220800`; the
original gameplay, graphics and levels are preserved. Generated gameplay media
and level imagery are shared under CC BY-NC-SA 4.0. Credit and these notices must
travel with exported images and histories. Level panoramas are assembled from
gameplay screenshots; browser maps add heat overlays. The All Novas view uses
unchanged player tiles and palette extracted from the pinned game source, arranged
as an attributed sprite sheet, to replay actual sampled search movement. Original artwork is
unchanged. Visible creator, source and license links accompany the live map,
replay and atlas, in addition to the footer and Credits dialog.

The deployed site includes the pinned Nova source archive, GPL text, pinned
QuickNES source, and browser frontend/build recipe in `licenses/`. The source
archive contains the upstream author/cameo credits. Harmony's complete source
and pinned build recipes are linked from the site; source is also available
alongside every release of this demo. `rust-dependencies.tar.gz` contains the
locked Rust dependency sources and their original license/copyright notices,
including miniz_oxide (MIT, Zlib or Apache-2.0) and adler2 (0BSD, MIT or
Apache-2.0). These permissive notices accompany the compiled search worker.

**QuickNES** by Shay Green and contributors, with the libretro port by its
contributors, is pinned to `26bb785c9deddb66a17717b21bb4e328f03ade32`:
https://github.com/libretro/QuickNES_Core/tree/26bb785c9deddb66a17717b21bb4e328f03ade32
The `nes_emu` library sources use **LGPL-2.1-or-later**. The libretro distribution
ships **GPL-2.0** terms, which this demo conservatively applies to the compiled
QuickNES browser core. Its C++ shim (`tools/frontend.cpp`) is
**GPL-2.0-or-later**, compatible with that distribution. See the shipped
`QuickNES.txt` and source-file notices. The compiled core is distributed with
its corresponding source and build recipe.
The separate browser interface and Harmony/Dissonance Rust code are
**AGPL-3.0-or-later**:
https://github.com/pH14/harmony

No game assets are relicensed as Harmony code. No advertising or payments are
part of this demonstration.
