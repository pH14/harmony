// SPDX-License-Identifier: AGPL-3.0-or-later
import { joust_observation } from "../../rust/pkg/twin_dragons_browser.js";

const LEVEL_COUNT = 49,
  SCREEN_WIDTH = 256,
  SCREEN_HEIGHT = 224,
  GAME_MODE = 0x00,
  GAME_PHASE = 0x02,
  PLAYING_MODE = 0x90,
  PLAYING_PHASE = 0x02,
  WAVE_ONES = 0x3a,
  WAVE_TENS = 0x3b,
  WAVE_ENEMIES = 0x73,
  WAVE_SPAWNED = 0x74,
  LIVES = 0xe9,
  START_LIVES = 4,
  OAM = 0x200,
  OAM_SPRITES = 64,
  HIDDEN_Y = 0xf8,
  SCREEN_TOP_CROP = 7,
  PLAY_TOP = 0x10,
  PLAY_BOTTOM = 0xc0,
  FLIP = 0x40,
  SETTLE_FRAMES = 360,
  WAVE_FRAME_LIMIT = 900,
  PLAYER_WIDTH = 16,
  PLAYER_HEIGHT = 24;

export function levelName(id) {
  return `Wave ${id + 1}`;
}
export function decode(memory) {
  return JSON.parse(joust_observation(memory));
}
function wave(ram) {
  return ram[WAVE_TENS] * 10 + ram[WAVE_ONES];
}
function hideSprites(ram) {
  for (let i = 0; i < OAM_SPRITES; i++) ram[OAM + i * 4] = HIDDEN_Y;
}
function playerSprites(ram) {
  const sprites = [];
  for (let i = 0; i < OAM_SPRITES; i++) {
    const at = OAM + i * 4, y = ram[at];
    if (y >= PLAY_TOP && y < PLAY_BOTTOM && (ram[at + 2] & 3) === 0) sprites.push({ y: y - SCREEN_TOP_CROP, x: ram[at + 3], flip: !!(ram[at + 2] & FLIP) });
  }
  return sprites;
}

function advanceTo(engine, target) {
  engine.boot(0);
  const ram = engine.ramView();
  for (let frame = 0; frame < WAVE_FRAME_LIMIT * target && wave(ram) < target; frame++) {
    if (ram[GAME_MODE] !== PLAYING_MODE || ram[GAME_PHASE] !== PLAYING_PHASE) throw new Error("Joust left the game before the requested wave");
    ram[WAVE_ENEMIES] = 0;
    ram[WAVE_SPAWNED] = 0;
    if (ram[LIVES] < 2) ram[LIVES] = 2;
    engine.run(0, 1, false);
  }
  if (wave(ram) !== target) throw new Error("Joust did not reach the requested wave");
}

export function boot(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT) throw new Error("Unknown Joust starting wave");
  advanceTo(engine, level + 1);
  engine.ramView()[LIVES] = START_LIVES;
  engine.run(0, 1, true);
}

export function buildMap(engine, level) {
  advanceTo(engine, level + 1);
  const ram = engine.ramView();
  for (let frame = 0; frame < SETTLE_FRAMES; frame++) {
    if (ram[LIVES] < 2) ram[LIVES] = 2;
    engine.run(0, 1, false);
  }
  for (let frame = 0; frame < 3; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const shown = engine.pixels();
  return { width: SCREEN_WIDTH, height: SCREEN_HEIGHT, data: new Uint8ClampedArray(shown.data), reached: SCREEN_WIDTH };
}

export function buildSprites(engine) {
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const sprites = playerSprites(ram);
  if (!sprites.length) throw new Error("Joust has no player sprite to capture");
  const left = Math.min(...sprites.map((s) => s.x)),
    top = Math.min(...sprites.map((s) => s.y)),
    flipped = sprites[0].flip,
    shown = engine.pixels().data;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    sheet = new Uint8ClampedArray(PLAYER_WIDTH * PLAYER_HEIGHT * 2 * 4);
  for (let y = 0; y < PLAYER_HEIGHT; y++)
    for (let x = 0; x < PLAYER_WIDTH; x++) {
      const px = left + x, py = top + y;
      if (px < 0 || px >= SCREEN_WIDTH || py < 0 || py >= SCREEN_HEIGHT) continue;
      const s = (py * SCREEN_WIDTH + px) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      const same = (y * PLAYER_WIDTH + x) * 4,
        mirrored = (y * PLAYER_WIDTH + PLAYER_WIDTH - 1 - x) * 4,
        rowLeft = PLAYER_HEIGHT * PLAYER_WIDTH * 4,
        shownRow = flipped ? rowLeft : 0,
        mirroredRow = flipped ? 0 : rowLeft;
      sheet.set(shown.subarray(s, s + 4), shownRow + same);
      sheet.set(shown.subarray(s, s + 4), mirroredRow + mirrored);
    }
  engine.restore(start);
  return { width: PLAYER_WIDTH, height: PLAYER_HEIGHT * 2, data: sheet };
}

export default {
  id: "joust",
  title: "Joust",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 32784,
    sha256: "9c47a4797cdf3e46f068020ca6cd6ebfc5ee06d3083b12ded656051a1368ee4e",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  grid: true,
  route: () => 0,
  entry: () => ({ x: SCREEN_WIDTH / 2, y: SCREEN_HEIGHT / 2 }),
  closeUp: { width: SCREEN_WIDTH, height: SCREEN_HEIGHT },
  pace: 300,
  maxActionFrames: 120,
  menuButtons: true,
  system: "NES",
  assets: "",
  screen: { width: SCREEN_WIDTH, height: SCREEN_HEIGHT },
  sprite: { width: PLAYER_WIDTH, height: PLAYER_HEIGHT, originX: 0, originY: -8, icon: [0, 0, PLAYER_WIDTH, PLAYER_HEIGHT] },
  controls: { a: "Flap", b: "Flap" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Joust</h2><p>Joust is © 1982 Williams Electronics Games, Inc. and © 1988 HAL America, Inc., sublicensed from Atari Corp. and licensed by Nintendo of America Inc. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The arena maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, name: levelName(id), label: levelName(id), width: SCREEN_WIDTH, height: SCREEN_HEIGHT })),
    levels: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, label: levelName(id), rooms: [id] })),
  },
  boot,
  buildMap,
  buildSprites,
  decode,
  campaign: true,
};
