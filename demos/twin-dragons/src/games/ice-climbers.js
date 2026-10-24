// SPDX-License-Identifier: AGPL-3.0-or-later
import { ice_climbers_observation } from "../../rust/pkg/twin_dragons_browser.js";

const LEVEL_COUNT = 32,
  LEVEL = 0x59,
  PHASE = 0x5a,
  PLAYER_X = 0x64,
  PLAYER_Y = 0x66,
  SCROLL_NAMETABLE = 0x0a,
  SCROLL_Y = 0x0b,
  INTRO_SCROLL = 0x13,
  START = 8,
  OAM = 0x200,
  SCREEN_WIDTH = 256,
  SCREEN_HEIGHT = 224,
  CROP_TOP = 8,
  START_TOP = 444,
  SCROLL_WRAP = 240,
  START_X = 0x38,
  START_Y = 0xc0,
  START_SCROLL = 0xcc;

export const MAP_WIDTH = SCREEN_WIDTH;
export const MAP_HEIGHT = START_TOP + SCREEN_HEIGHT;
export const MAP_SIZES = Array.from({ length: LEVEL_COUNT }, () => ({ width: MAP_WIDTH, height: MAP_HEIGHT }));

export function levelName(id) {
  return `Mountain ${id + 1}`;
}

export function decode(ram) {
  return JSON.parse(ice_climbers_observation(ram));
}

function playable(ram, level) {
  return (
    ram[LEVEL] === level && ram[PHASE] === 1 && ram[PLAYER_X] === START_X && ram[PLAYER_Y] === START_Y && ram[SCROLL_Y] === START_SCROLL
  );
}

function hideSprites(ram) {
  for (let i = OAM; i < OAM + 0x100; i += 4) ram[i] = 0xf8;
}

function title(engine, level) {
  engine.reset();
  const ram = engine.ramView();
  engine.run(0, 120, true);
  ram[LEVEL] = level;
  engine.run(START, 2, true);
  return ram;
}

function boot(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT) throw new Error("Unknown Ice Climber starting mountain");
  const ram = title(engine, level);
  for (let frames = 0; frames < 2400; frames++) {
    if (playable(ram, level)) return;
    engine.run(0, 1, true);
  }
  throw new Error("Ice Climber did not reach the mountain");
}

function bootPlayable(engine, level) {
  engine.boot(level);
}

export function buildMap(engine, level) {
  const ram = title(engine, level), data = new Uint8ClampedArray(MAP_WIDTH * MAP_HEIGHT * 4);
  let top = START_TOP, previous = -1, scrolled = false, first = true;
  const copy = () => {
    const pixels = engine.pixelView();
    for (let row = 0; row < SCREEN_HEIGHT; row++) {
      const y = top + row;
      if (y < 0 || y >= MAP_HEIGHT) continue;
      data.set(pixels.subarray(row * SCREEN_WIDTH * 4, (row + 1) * SCREEN_WIDTH * 4), y * MAP_WIDTH * 4);
    }
  };
  for (let frame = 0; frame < 1200 && top > 0; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
    const scroll = ram[INTRO_SCROLL];
    if (frame < 140) {
      previous = scroll;
      continue;
    }
    if (!scrolled && scroll === previous) {
      if (first) copy();
      first = false;
      continue;
    }
    scrolled = true;
    top -= (previous - scroll + SCROLL_WRAP) % SCROLL_WRAP;
    previous = scroll;
    copy();
  }
  return { width: MAP_WIDTH, height: MAP_HEIGHT, data, reached: MAP_HEIGHT };
}

export function buildSprites(engine) {
  bootPlayable(engine, 0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = ram[PLAYER_X],
    sy = ram[PLAYER_Y] + 1 - CROP_TOP;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    width = 16,
    height = 24,
    sheet = new Uint8ClampedArray(width * height * 2 * 4);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const s = ((sy + y) * SCREEN_WIDTH + sx + x) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      sheet.set(shown.subarray(s, s + 4), (y * width + x) * 4);
      sheet.set(shown.subarray(s, s + 4), ((height + y) * width + width - 1 - x) * 4);
    }
  engine.restore(start);
  return { width, height: height * 2, data: sheet };
}

export default {
  id: "ice-climbers",
  title: "Ice Climber",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 24592,
    sha256: "491898c9b0bf6b242d08bb0841141ca404f8e54383a1814cdb7feab451da8701",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  vertical: true,
  pace: 300,
  maxActionFrames: 120,
  menuButtons: false,
  system: "NES",
  assets: "",
  screen: { width: SCREEN_WIDTH, height: SCREEN_HEIGHT },
  closeUp: { width: SCREEN_WIDTH, height: 300 },
  sprite: { width: 16, height: 24, originX: 0, originY: 0, icon: [0, 0, 16, 24] },
  controls: { a: "Jump", b: "Hammer" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Ice Climber</h2><p>Ice Climber is © Nintendo. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The mountain maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, name: levelName(id), label: levelName(id), ...MAP_SIZES[id] })),
    levels: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, label: levelName(id), rooms: [id] })),
  },
  buildMap,
  buildSprites,
  decode,
  campaign: true,
  boot,
};
