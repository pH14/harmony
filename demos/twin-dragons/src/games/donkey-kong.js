// SPDX-License-Identifier: AGPL-3.0-or-later
import { donkey_kong_observation } from "../../rust/pkg/twin_dragons_browser.js";

const STAGE = 0x53,
  ROUND = 0x54,
  DEMO = 0x58,
  MARIO_Y = 0x47,
  STATUS = 0x96,
  OAM = 0x200,
  OAM_END = 0x300,
  HIDDEN = 0xf8,
  START = 8,
  STANDING = 1,
  WIDTH = 256,
  HEIGHT = 224,
  SPRITE = 16;

export const STAGES = [1, 3, 4];
const NAMES = [
  "25m Barrels",
  "75m Elevators",
  "100m Rivets",
  "Round 2+ Barrels",
  "Round 2+ Elevators",
  "Round 2+ Rivets",
];
const LEVEL_COUNT = NAMES.length;
const MASKS = [
  [[0, 16, 256, 23], [160, 23, 256, 41]],
  [[0, 16, 256, 23], [160, 23, 256, 41], [30, 24, 86, 56]],
  [[0, 16, 256, 23], [160, 23, 256, 41], [104, 24, 156, 56]],
];

export function levelName(id) {
  return NAMES[id];
}
export function decode(ram) {
  return JSON.parse(donkey_kong_observation(ram));
}

function playable(ram) {
  return ram[MARIO_Y] !== 0 && ram[STATUS] === STANDING && ram[DEMO] === 0 && ram[OAM] !== HIDDEN;
}

function boot(engine, id) {
  if (!Number.isInteger(id) || id < 0 || id >= LEVEL_COUNT) throw new Error("Unknown Donkey Kong starting stage");
  const ram = engine.ramView();
  engine.run(0, 100, true);
  engine.run(START, 3, true);
  engine.run(0, 30, true);
  ram[STAGE] = STAGES[id % STAGES.length];
  ram[ROUND] = id >= STAGES.length ? 1 : 0;
  for (let frames = 0; frames < 1500; frames++) {
    if (playable(ram)) {
      engine.run(0, 30, true);
      return;
    }
    engine.run(0, 1, true);
  }
  throw new Error("Donkey Kong did not reach the stage");
}

function hideSprites(ram) {
  for (let i = OAM; i < OAM_END; i += 4) ram[i] = HIDDEN;
}

export function buildMap(engine, id) {
  engine.boot(id);
  const ram = engine.ramView();
  engine.run(0, 240, true);
  for (let frame = 0; frame < 3; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const data = new Uint8ClampedArray(engine.pixelView());
  for (const [left, top, right, bottom] of MASKS[id % MASKS.length])
    for (let y = top; y < bottom; y++)
      for (let x = left; x < right; x++) data.set([0, 0, 0, 255], (y * WIDTH + x) * 4);
  return { width: WIDTH, height: HEIGHT, data };
}

export function buildSprites(engine) {
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = ram[OAM + 3],
    sy = ram[OAM] + 1;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    ram[OAM] = HIDDEN;
    ram[OAM + 4] = HIDDEN;
    ram[OAM + 8] = HIDDEN;
    ram[OAM + 12] = HIDDEN;
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    sheet = new Uint8ClampedArray(SPRITE * SPRITE * 2 * 4),
    faceLeft = ram[OAM + 2] & 0x40;
  for (let y = 0; y < SPRITE; y++)
    for (let x = 0; x < SPRITE; x++) {
      const s = ((sy + y) * WIDTH + sx + x) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      const right = (y * SPRITE + x) * 4,
        left = ((SPRITE + y) * SPRITE + SPRITE - 1 - x) * 4;
      sheet.set(shown.subarray(s, s + 4), faceLeft ? left : right);
      sheet.set(shown.subarray(s, s + 4), faceLeft ? right : left);
    }
  engine.restore(start);
  return { width: SPRITE, height: SPRITE * 2, data: sheet };
}

export default {
  id: "donkey-kong",
  title: "Donkey Kong",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 24592,
    sha256: "226c6f698f60b0a07157bd031004820789469884615fef7bdd862c3157e89d27",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  pace: 300,
  maxActionFrames: 120,
  menuButtons: false,
  system: "NES",
  assets: "",
  screen: { width: WIDTH, height: HEIGHT },
  sprite: { width: SPRITE, height: SPRITE, originX: 0, originY: 0, icon: [0, 0, SPRITE, SPRITE] },
  controls: { a: "Jump", b: "Jump" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Donkey Kong</h2><p>Donkey Kong is © Nintendo. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The stage maps and the Mario sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: NAMES.map((name, id) => ({ id, name, label: name, width: WIDTH, height: HEIGHT })),
    levels: NAMES.map((label, id) => ({ id, label, rooms: [id] })),
  },
  buildMap,
  buildSprites,
  decode,
  mapProgress: (observation) => HEIGHT - observation.y,
  campaign: true,
  boot,
};
