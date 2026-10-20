// SPDX-License-Identifier: AGPL-3.0-or-later
import { famidash_groups, famidash_level_select_tape, famidash_observation, famidash_start_level_tape } from "../../rust/pkg/twin_dragons_browser.js";
import { loadRust } from "../rust.js";

await loadRust();

const LEVEL = 0x47b,
  ROM_BYTES = 786448,
  RELEASE = "https://github.com/tfdsoft/famidash/releases/tag/1.2.8",
  COMMIT = "https://github.com/tfdsoft/famidash/tree/0df680d0e02f0ccb438f1224d162a0ab073b7204";

export const GROUPS = JSON.parse(famidash_groups());
const START = JSON.parse(famidash_start_level_tape());

const escape = (text) => text.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);

async function credits(base) {
  const music = await fetch(new URL("famidash/music.json", base))
    .then((response) => (response.ok ? response.json() : []))
    .catch(() => []);
  const song = ({ title, original, cover }) =>
    `<li>${escape(title)}: ${escape(original)}${cover ? `, arranged by ${escape(cover)}` : ""}</li>`;
  const artists = music.map(({ name, songs }) => `<h3>${escape(name)}</h3><ul>${songs.map(song).join("")}</ul>`).join("");
  return `<span class="eyebrow">CREDITS & LICENSING</span><h2>Famidash</h2><p>Created by <a href="https://github.com/tfdsoft">TFDSoft</a>. Copyright 2023-2024 TFDSoft. <a href="${RELEASE}">Release 1.2.8</a>, built from <a href="${COMMIT}">commit 0df680d</a>. The game is under the <a href="licenses/famidash-LICENSE.txt">MIT license</a>. This demo runs Harmony’s repaired builds of the main game, B-Sides, C-Sides and D-Sides. The changes repair collision handling and specific level openings. <a href="licenses/famidash-repairs/README.md">Repair sources and validation</a>.</p><p>Famidash is an unofficial fan demake of Geometry Dash by RobTop Games. It is not affiliated with or endorsed by RobTop Games. The level designs follow levels from Geometry Dash and its community.</p><p>The music is arranged from tracks by these artists, as named in the release’s music metadata:</p>${artists}<p>See the <a href="licenses/famidash-CREDITS.md">music credits</a> and the music metadata for the <a href="licenses/famidash-music-main.json5">main game</a>, <a href="licenses/famidash-music-b-sides.json5">B-Sides</a>, <a href="licenses/famidash-music-c-sides.json5">C-Sides</a> and <a href="licenses/famidash-music-d-sides.json5">D-Sides</a>. The level maps and the player sprite sheet are captured from the game’s screens and composed into images; browser maps add heat overlays. This demonstration is not endorsed by the authors.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a> and <a href="licenses/quicknes-four-screen-state.patch">four-screen state patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`;
}

function group(entry) {
  const levels = new Set(entry.levels.map((level) => level.id));
  return {
    id: entry.game,
    title: "Famidash",
    family: GROUPS[0].game,
    group: entry.name,
    levels: entry.levels.map(({ id, name }) => ({ id, label: name, rooms: [id] })),
    core: "quicknes",
    rom: { file: `famidash/${entry.rom}`, bytes: ROM_BYTES, sha256: entry.rom_sha256 },
    ramBytes: 2048,
    levelCount: entry.level_count,
    maxHealth: 0,
    pace: 120,
    maxActionFrames: 120,
    menuButtons: false,
    system: "NES",
    assets: `famidash/${entry.id}/`,
    screen: { width: 256, height: 224 },
    sprite: { width: 24, height: 24, originX: 4, originY: 4, icon: [4, 4, 16, 16] },
    controls: { a: "Jump" },
    attribution: `<a href="https://github.com/tfdsoft/famidash">Famidash</a> by TFDSoft, <a href="licenses/famidash-LICENSE.txt">MIT</a>`,
    credits,
    decode: (ram) => JSON.parse(famidash_observation(entry.game, ram)),
    campaign: true,
    boot(engine, level) {
      if (!levels.has(level)) throw new Error(`${entry.name} has no Famidash level ${level}`);
      for (const { buttons, frames } of JSON.parse(famidash_level_select_tape(entry.game))) engine.run(buttons, frames, true);
      if (engine.ramView()[LEVEL] !== level) engine.ramView()[LEVEL] = level;
      for (const { buttons, frames } of START) engine.run(buttons, frames, true);
      const o = engine.observation();
      if (o.loading || o.level !== level || o.percent !== 0)
        throw new Error(`Setup did not reach ${entry.name} level ${level}`);
    },
  };
}

export const FAMIDASH = GROUPS.map(group);
export default FAMIDASH[0];
