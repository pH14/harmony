// SPDX-License-Identifier: AGPL-3.0-or-later
const LEVEL_COUNT = 3,
  GAMESTATE = 0xa5,
  SCENE = 0xa6,
  LEVEL = 0xa7,
  UNLOCKED = 0xa8,
  SELECTION = 0xaa,
  TIME = 0xb0,
  IN_LEVEL = 4,
  LEVEL_SELECT = 3,
  WIN = 6,
  UNLOCK_NOTICE = 9,
  ENDING = 10,
  START = 8,
  RIGHT = 0x80,
  A = 1,
  SPRITE_LEFT = 24,
  SPRITE_CENTER = 155;

export const MAP_HEIGHTS = [2000, 2896, 3792];
export const LEVEL_NAMES = ["Plains", "Clouds", "Space"];

export function climb(ram) {
  return ram[0x233] * ram[0x237] + ram[0x230];
}

export function mapPoint(ram) {
  const height = MAP_HEIGHTS[Math.min(2, Math.max(0, ram[LEVEL] - 1))];
  return [Math.max(0, ram[0xc2] - SPRITE_LEFT), Math.max(0, height + ram[0xc3] - SPRITE_CENTER - climb(ram))];
}

export function decode(ram) {
  if (ram.length !== 0x400) throw new Error("Invalid Tobu Tobu Girl memory");
  const gamestate = ram[GAMESTATE],
    scene = ram[SCENE],
    level = Math.max(0, ram[LEVEL] - 1),
    playing = gamestate === IN_LEVEL && scene === 0;
  const [x, y] = mapPoint(ram);
  return {
    x,
    y,
    health: playing || gamestate === ENDING ? 1 : 0,
    level,
    gamestate,
    scene,
    climb: climb(ram),
    time: ram[TIME],
    fuel: ram[0x23a],
    goal: ram[0x235] !== 0,
    loading: !playing,
    cleared: gamestate === ENDING ? LEVEL_COUNT : level,
  };
}

function playing(ram, level) {
  return ram[GAMESTATE] === IN_LEVEL && ram[SCENE] === 0 && ram[LEVEL] === level + 1 && ram[TIME] !== 0;
}

function drive(ram, run, target, limit) {
  const actions = [];
  let released = 2;
  for (let frames = 0; frames < limit; frames++) {
    const gamestate = ram[GAMESTATE];
    if (gamestate === ENDING) return { actions, won: true };
    if (playing(ram, target)) return { actions, won: false };
    let buttons = 0;
    if (released >= 2) {
      if (gamestate === WIN || gamestate === UNLOCK_NOTICE) buttons = A;
      else if (gamestate === LEVEL_SELECT) buttons = ram[SELECTION] < target + 1 ? RIGHT : START;
      else if (gamestate !== IN_LEVEL) buttons = START;
    }
    run(buttons, 1);
    released = buttons ? 0 : released + 1;
    const last = actions[actions.length - 1];
    if (last !== undefined && last >> 8 === buttons && (last & 255) < 255) actions[actions.length - 1]++;
    else actions.push((buttons << 8) | 1);
  }
  throw new Error("Tobu Tobu Girl did not reach the next level");
}

export default {
  id: "tobu-tobu-girl",
  title: "Tobu Tobu Girl Deluxe",
  core: "sameboy",
  rom: {
    file: "tobu-tobu-girl/tobu-tobu-girl.gb",
    bytes: 262144,
    sha256: "839a2a3df9785be71757bed4229474ed8aafe37576efd1d394fb61ff7d314f08",
  },
  ramBytes: 0x400,
  levelCount: LEVEL_COUNT,
  vertical: true,
  pace: 120,
  maxActionFrames: 255,
  menuButtons: true,
  system: "Game Boy",
  assets: "tobu-tobu-girl/",
  screen: { width: 160, height: 144 },
  closeUp: { width: 144, height: 360 },
  sprite: { width: 16, height: 16, originX: 0, originY: 0, icon: [0, 0, 16, 16] },
  controls: { a: "Dash", b: "Jetpack" },
  lede: `Watch Harmony play the Game Boy game <a href="https://tangramgames.dk/tobutobugirldx/">Tobu Tobu Girl Deluxe</a> right in your browser.`,
  attribution: `<a href="https://tangramgames.dk/tobutobugirldx/">Tobu Tobu Girl Deluxe</a> by Tangram Games · Artwork <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>, modified`,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Tobu Tobu Girl Deluxe</h2><p>Created by <a href="https://tangramgames.dk/">Tangram Games</a>. <a href="https://github.com/SimonLarsen/tobutobugirl-dx/tree/4985d4d282aa43229f250ffa3b0d86998ebd55d3">Source at commit 4985d4d</a>. The game’s code is under the <a href="licenses/tobu-tobu-girl-LICENSE.txt">MIT license</a>. Its images, text, sound and music are under <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>.</p><p>This build is compiled from that source with a pinned toolchain. Modified: the level maps and the player sprite sheet are captured from the game’s screens and composed into images; browser maps stretch each level background to the level’s height and add heat overlays. This demonstration is not endorsed by the authors.</p><p>SameBoy by Lior Halphon runs the game, under the <a href="licenses/sameboy-LICENSE.txt">Expat license</a>. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/sameboy-source.tar.gz">SameBoy source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  decode,
  ended: (ram) => ram[GAMESTATE] !== IN_LEVEL || ram[SCENE] !== 0,
  levelComplete: (ram) => ram[GAMESTATE] === IN_LEVEL && ram[SCENE] === 2,
  transition(engine) {
    const ram = engine.ramView();
    return drive(ram, engine.step, ram[LEVEL], 6000).actions;
  },
  motion(ram) {
    const valid = ram[GAMESTATE] === IN_LEVEL && ram[SCENE] === 0;
    return [valid ? ram[LEVEL] - 1 : 65535, ...mapPoint(ram), ram[0xc4] === 4 ? 1 : 0];
  },
  boot(engine, level) {
    if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT)
      throw new Error("Unknown Tobu Tobu Girl starting level");
    const run = (buttons, frames) => {
      engine.run(buttons, frames, true);
      const ram = engine.ramView();
      if (ram[GAMESTATE] === LEVEL_SELECT && ram[UNLOCKED] < level + 1) ram[UNLOCKED] = level + 1;
    };
    drive(engine.ramView(), run, level, 4000);
  },
};
