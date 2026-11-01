// SPDX-License-Identifier: AGPL-3.0-or-later
import { pac_man_observation } from "../../rust/pkg/twin_dragons_browser.js";

const MODE = 0x3f,
  PAC_X = 0x1a,
  PAC_Y = 0x1c,
  LEVEL = 0x68,
  OAM = 0x700,
  PLAY = 4,
  START = 8,
  LEFT = 0x40,
  LEVEL_COUNT = 6,
  MAZES_PER_LEVEL = 4,
  MAP_WIDTH = 176,
  MAP_HEIGHT = 224,
  SPRITE_SIZE = 16,
  SPRITE_LEFT = 12,
  SPRITE_TOP = 4,
  POSES = 4;

export function routeOrder() {
  return 0;
}

export function entry() {
  return { x: 91, y: 172 };
}

export function levelName(id) {
  if (id === LEVEL_COUNT - 1) return `Mazes ${id * MAZES_PER_LEVEL + 1}+`;
  return `Mazes ${id * MAZES_PER_LEVEL + 1}-${(id + 1) * MAZES_PER_LEVEL}`;
}

export function decode(memory) {
  return JSON.parse(pac_man_observation(memory));
}

function toPlay(engine, limit = 400) {
  const ram = engine.ramView();
  for (let frame = 0; frame < limit && ram[MODE] !== PLAY; frame++) engine.run(0, 1, true);
  if (ram[MODE] !== PLAY) throw new Error("Pac-Man did not reach the maze");
}

function boot(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT) throw new Error("Unknown Pac-Man starting level");
  engine.run(0, 300, true);
  engine.run(START, 2, true);
  engine.run(0, 100, true);
  engine.run(START, 2, true);
  engine.run(0, 2, true);
  engine.ramView()[LEVEL] = level * MAZES_PER_LEVEL;
  toPlay(engine);
}

function hideSprites(ram) {
  for (let i = OAM; i < OAM + 0x100; i += 4) ram[i] = 0xf8;
}

let cachedMap = new WeakMap();

function drawMap(engine) {
  engine.boot(0);
  const ram = engine.ramView();
  for (let frame = 0; frame < 3; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const pixels = engine.pixelView(),
    width = 256,
    data = new Uint8ClampedArray(MAP_WIDTH * MAP_HEIGHT * 4);
  for (let y = 0; y < MAP_HEIGHT; y++)
    data.set(pixels.subarray(y * width * 4, (y * width + MAP_WIDTH) * 4), y * MAP_WIDTH * 4);
  return data;
}

export function buildMap(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= LEVEL_COUNT) throw new Error("Unknown Pac-Man maze");
  let data = cachedMap.get(engine);
  if (!data) {
    data = drawMap(engine);
    cachedMap.set(engine, data);
  }
  return { width: MAP_WIDTH, height: MAP_HEIGHT, data: data.slice(), reached: MAP_WIDTH };
}

function pacman(engine) {
  const ram = engine.ramView(),
    before = engine.capture();
  engine.run(LEFT, 1, true);
  const shown = engine.pixels().data,
    x = ram[PAC_X] - SPRITE_LEFT,
    y = ram[PAC_Y] - SPRITE_TOP;
  engine.restore(before);
  hideSprites(ram);
  engine.run(LEFT, 1, true);
  const hidden = engine.pixels().data,
    out = new Uint8ClampedArray(SPRITE_SIZE * SPRITE_SIZE * 4);
  let count = 0;
  for (let row = 0; row < SPRITE_SIZE; row++)
    for (let col = 0; col < SPRITE_SIZE; col++) {
      const at = ((y + row) * 256 + x + col) * 4;
      if (shown[at] === hidden[at] && shown[at + 1] === hidden[at + 1] && shown[at + 2] === hidden[at + 2]) continue;
      out.set(shown.subarray(at, at + 3), (row * SPRITE_SIZE + col) * 4);
      out[(row * SPRITE_SIZE + col) * 4 + 3] = 255;
      count++;
    }
  return { out, count };
}

function turned(source, move) {
  const out = new Uint8ClampedArray(source.length);
  for (let y = 0; y < SPRITE_SIZE; y++)
    for (let x = 0; x < SPRITE_SIZE; x++) {
      const [tx, ty] = move(x, y);
      out.set(source.subarray((y * SPRITE_SIZE + x) * 4, (y * SPRITE_SIZE + x) * 4 + 4), (ty * SPRITE_SIZE + tx) * 4);
    }
  return out;
}

export function buildSprites(engine) {
  engine.boot(0);
  const start = engine.capture(),
    shapes = [];
  for (let frame = 0; frame < 24; frame++) shapes.push(pacman(engine));
  engine.restore(start);
  shapes.sort((a, b) => a.count - b.count);
  const open = shapes[0],
    closed = shapes[shapes.length - 1],
    size = SPRITE_SIZE,
    sheet = new Uint8ClampedArray(size * POSES * size * 2 * 4),
    place = (column, row, pixels) => {
      for (let y = 0; y < size; y++)
        sheet.set(pixels.subarray(y * size * 4, (y + 1) * size * 4), ((row * size + y) * size * POSES + column * size) * 4);
    },
    turns = [
      (x, y) => [size - 1 - x, y],
      (x, y) => [x, y],
      (x, y) => [size - 1 - y, x],
      (x, y) => [y, size - 1 - x],
    ];
  [open, closed].forEach((shape, row) => turns.forEach((move, column) => place(column, row, turned(shape.out, move))));
  return { width: size * POSES, height: size * 2, data: sheet };
}

export default {
  id: "pac-man",
  title: "Pac-Man",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 24592,
    sha256: "a72c6f56b14be0f89ac08a3417b424565cdf0e7b53a501b786b25fa704643d80",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  levelsCleared: (observation) => Math.min(LEVEL_COUNT, Math.floor(observation.cleared / MAZES_PER_LEVEL)),
  grid: true,
  route: routeOrder,
  entry,
  pace: 300,
  maxActionFrames: 255,
  menuButtons: false,
  system: "NES",
  assets: "",
  screen: { width: 256, height: 224 },
  closeUp: { width: MAP_WIDTH, height: MAP_HEIGHT },
  sprite: { width: SPRITE_SIZE, height: SPRITE_SIZE, originX: 0, originY: -8, icon: [0, 0, SPRITE_SIZE, SPRITE_SIZE] },
  controls: { a: "Unused", b: "Unused" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Pac-Man</h2><p>Pac-Man is © Bandai Namco Entertainment. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The maze and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Bandai Namco.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, name: levelName(id), label: levelName(id), width: MAP_WIDTH, height: MAP_HEIGHT })),
    levels: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, label: levelName(id), rooms: [id] })),
  },
  buildMap,
  buildSprites,
  decode,
  campaign: true,
  boot,
};
