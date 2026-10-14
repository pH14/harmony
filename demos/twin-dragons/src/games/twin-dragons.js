// SPDX-License-Identifier: AGPL-3.0-or-later
const LEVEL_COUNT = 3,
  NEXT_LEVEL = 0x35,
  TOUCHED_GOAL = 0x4e3,
  BOOT = [
    [0, 60],
    [8, 2],
    [0, 240],
  ];

export function settled(gamestate, level, next) {
  return gamestate === 1 && (next === level + 1 || (level === LEVEL_COUNT - 1 && next === 0xff));
}
export function levelsCleared(o) {
  if (o.gamestate === 1 && !o.loading && o.goal) return o.level + 1;
  return o.gamestate === 1 || o.gamestate === 2 ? o.level : 0;
}
export function decode(ram) {
  if (ram.length !== 2048) throw new Error("Invalid Twin Dragons memory");
  const o = {
    x: ram[0x52] * 256 + ram[0x57] + ram[0x62],
    y: ram[0x63],
    health: ram[0x75],
    level: ram[0x42],
    next_level: ram[NEXT_LEVEL],
    gamestate: ram[0x9e],
    goal: ram[TOUCHED_GOAL] !== 0,
    on_ground: ram[0x6b] !== 0,
    facing: ram[0x74],
  };
  o.loading = !settled(o.gamestate, o.level, o.next_level);
  o.cleared = levelsCleared(o);
  return o;
}
export function playerPose(ram) {
  const animation = ram[0x71], frame = ram[0x72];
  const pose = animation === 1 ? 1 + Math.min(3, frame) : animation === 2 ? 5 : animation === 3 ? 6 : 0;
  return pose * 2 + (ram[0x74] ? 1 : 0);
}

export default {
  id: "twin-dragons",
  title: "Twin Dragons",
  core: "quicknes",
  rom: {
    file: "twin-dragons.nes",
    bytes: 40976,
    sha256: "7b64267068caab5379b833336e836d6de49c7d37cc5a4341b83d07cbccf0ef73",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  maxHealth: 3,
  pace: 85,
  maxActionFrames: 120,
  menuButtons: false,
  system: "NES",
  assets: "",
  screen: { width: 256, height: 224 },
  sprite: { width: 32, height: 24, originX: 8, originY: 4, icon: [4, 2, 20, 19] },
  controls: { a: "Jump", b: "Breathe fire" },
  attribution: `<a href="https://github.com/Garydos/twin-dragons-nes">Twin Dragons</a> by Garydos · Artwork by <a href="https://opengameart.org/content/twin-dragons">surt</a>, <a href="https://creativecommons.org/licenses/by/3.0/">CC BY 3.0</a>, modified`,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Twin Dragons</h2><p>Created by <a href="https://github.com/Garydos">Garydos</a>. <a href="https://github.com/Garydos/twin-dragons-nes/tree/bcf27d0c5117a2aac19e6c33bdb3c776ada069fb">Source at commit bcf27d0</a>. The game’s code is under the <a href="licenses/twin-dragons-LICENSE.txt">zlib license</a>.</p><p>This build is altered: the sound-effect data is zeroed. The sound effects by Shiru are not included.</p><p>Some artwork by surt, <a href="https://opengameart.org/content/twin-dragons">opengameart.org/content/twin-dragons</a>, <a href="https://creativecommons.org/licenses/by/3.0/">CC BY 3.0</a>. Modified: the level maps and the player sprite sheet are rendered from the game’s tiles with its palette and composed into images; browser maps add heat overlays. Music by Garydos, <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>. FamiTone2 by Shiru and FamiTone5 by nesdoug are public domain. This demonstration is not endorsed by the authors.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> \u00b7 <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  decode,
  ended: (ram) => ram[0x75] === 0 || ram[0x9e] === 0,
  motion(ram) {
    const level = ram[0x42];
    const valid = settled(ram[0x9e], level, ram[0x35]) && ram[0x75] !== 0;
    return [valid ? level : 65535, ram[0x52] * 256 + ram[0x57] + ram[0x62], ram[0x63], playerPose(ram)];
  },
  boot(engine, level) {
    if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT)
      throw new Error("Unknown Twin Dragons starting level");
    for (const step of BOOT) engine.run(...step, true);
    if (level > 0) {
      const ram = engine.ramPointer();
      engine.mod.HEAPU8[ram + NEXT_LEVEL] = level;
      engine.mod.HEAPU8[ram + TOUCHED_GOAL] = 1;
      for (let frames = 0; frames < 600; frames++) {
        engine.run(0, 1, true);
        const o = engine.observation();
        if (!o.loading && o.level === level && !o.goal && o.on_ground) break;
      }
    } else
      for (let frames = 0; frames < 120 && !engine.observation().on_ground; frames++)
        engine.run(0, 1, true);
    const o = engine.observation();
    if (o.loading || o.level !== level || o.health !== 3 || o.goal || !o.on_ground)
      throw new Error("Setup did not reach the selected Twin Dragons level");
  },
};
