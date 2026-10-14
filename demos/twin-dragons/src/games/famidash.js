// SPDX-License-Identifier: AGPL-3.0-or-later
import { famidash_level_select_tape, famidash_observation } from "../../rust/pkg/twin_dragons_browser.js";

const LEVEL = 0x47b,
  A = 1;

export const LEVEL_NAMES = [
  "Stereo Madness",
  "Back on Track",
  "Polargeist",
  "Dry Out",
  "Base After Base",
  "Can't Let Go",
  "Jumper",
  "Time Machine",
  "Cycles",
  "xStep",
  "Clutterfunk",
  "Theory of Everything",
  "Electroman Adventures",
  "Clubstep",
  "Electrodynamix",
  "Hexagon Force",
  "Blast Processing",
  "Theory of Everything 2",
  "Geometrical Dominator",
  "Deadlocked",
  "Fingerdash",
  "Dash",
];

export function decode(ram) {
  return JSON.parse(famidash_observation(ram));
}

export default {
  id: "famidash",
  title: "Famidash",
  core: "quicknes",
  rom: {
    file: "famidash/famidash.nes",
    bytes: 786448,
    sha256: "7acf068a406d551ff18afa3fc1ed228ff90f72651c4a90a3862b997240bb9b58",
  },
  ramBytes: 2048,
  levelCount: LEVEL_NAMES.length,
  maxHealth: 0,
  pace: 120,
  maxActionFrames: 120,
  menuButtons: false,
  system: "NES",
  assets: "famidash/",
  screen: { width: 256, height: 224 },
  sprite: { width: 24, height: 24, originX: 4, originY: 4, icon: [4, 4, 16, 16] },
  controls: { a: "Jump" },
  attribution: `<a href="https://github.com/tfdsoft/famidash">Famidash</a> by TFDSoft, <a href="licenses/famidash-LICENSE.txt">MIT</a> · An unofficial fan demake of RobTop’s Geometry Dash`,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Famidash</h2><p>Created by <a href="https://github.com/tfdsoft">TFDSoft</a>. Copyright 2023-2024 TFDSoft. <a href="https://github.com/tfdsoft/famidash/releases/tag/1.2.8">Release 1.2.8</a>, built from <a href="https://github.com/tfdsoft/famidash/tree/0df680d0e02f0ccb438f1224d162a0ab073b7204">commit 0df680d</a>. The game is under the <a href="licenses/famidash-LICENSE.txt">MIT license</a>. This demo runs the official release ROM unchanged.</p><p>Famidash is an unofficial fan demake of Geometry Dash by RobTop Games. It is not affiliated with or endorsed by RobTop Games. The level designs follow Geometry Dash’s official levels.</p><p>The music is arranged from tracks by ForeverBound, DJVI, Step, Waterflame, Dj-Nate and OcularNebula; see the <a href="licenses/famidash-CREDITS.md">music credits</a>. The level maps and the player sprite sheet are captured from the game’s screens and composed into images; browser maps add heat overlays. This demonstration is not endorsed by the authors.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a> and <a href="licenses/quicknes-four-screen-state.patch">four-screen state patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  decode,
  campaign: true,
  boot(engine, level) {
    if (!Number.isInteger(level) || level < 0 || level >= LEVEL_NAMES.length)
      throw new Error("Unknown Famidash starting level");
    for (const { buttons, frames } of JSON.parse(famidash_level_select_tape())) engine.run(buttons, frames, true);
    engine.ramView()[LEVEL] = level;
    engine.run(A, 1, true);
    engine.run(0, 90, true);
    const o = engine.observation();
    if (o.loading || o.level !== level || o.percent !== 0)
      throw new Error("Setup did not reach the selected Famidash level");
  },
};
