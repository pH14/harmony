// SPDX-License-Identifier: AGPL-3.0-or-later
import { zelda_observation } from "../../rust/pkg/twin_dragons_browser.js";

const RAM_LEVEL = 0x10,
  RAM_MODE = 0x12,
  RAM_ROOM = 0xeb,
  RAM_OBJECT_X = 0x70,
  RAM_OBJECT_Y = 0x84,
  RAM_OAM = 0x200,
  MODE_PLAY = 5,
  MODE_LEVEL_LOAD = 2,
  START_ROOM = 0x77,
  COLUMNS = 16,
  ROWS = 8,
  LEVELS = 10,
  ROOM_WIDTH = 256,
  ROOM_HEIGHT = 168,
  PLAY_TOP = 56,
  FRAME_WIDTH = 256,
  FRAME_HEIGHT = 224,
  LINK_SIZE = 16,
  ROM_HEADER = 16,
  LEVEL_INFO_START = 103420,
  LEVEL_INFO_SIZE = 252,
  LEVEL_INFO_ROTATION = 0x2d,
  LEVEL_INFO_MASK = 0x3f;

const ENTRANCES = [START_ROOM, 0x73, 0x7d, 0x7c, 0x71, 0x76, 0x79, 0x79, 0x7e, 0x76];

const NAMES = ["Overworld", ...Array.from({ length: LEVELS - 1 }, (_, i) => `Dungeon ${i + 1}`)];

export const MAP_SIZES = Array.from({ length: LEVELS }, () => ({
  width: COLUMNS * ROOM_WIDTH,
  height: ROWS * ROOM_HEIGHT,
}));

export function decode(memory) {
  return JSON.parse(zelda_observation(memory));
}

function cellOf(room) {
  return [room & 15, room >> 4];
}

function distances(start) {
  const far = new Map([[start, 0]]), queue = [start];
  while (queue.length) {
    const at = queue.shift();
    const [cx, cy] = cellOf(at);
    for (const [dx, dy] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
      const nx = cx + dx, ny = cy + dy;
      if (nx < 0 || ny < 0 || nx >= COLUMNS || ny >= ROWS) continue;
      const next = ny * COLUMNS + nx;
      if (far.has(next)) continue;
      far.set(next, far.get(at) + 1);
      queue.push(next);
    }
  }
  return far;
}
const ROUTES = ENTRANCES.map(distances);

export function routeOrder(level, x, y) {
  if (level < 0 || level >= LEVELS) return -1;
  const cx = Math.floor(x / ROOM_WIDTH), cy = Math.floor(y / ROOM_HEIGHT);
  const steps = ROUTES[level].get(cy * COLUMNS + cx);
  return steps === undefined ? -1 : steps * 256 + ((x % ROOM_WIDTH) + (y % ROOM_HEIGHT)) / 2;
}

export function entry(level) {
  const [cx, cy] = cellOf(ENTRANCES[level] ?? START_ROOM);
  return { x: cx * ROOM_WIDTH + ROOM_WIDTH / 2, y: cy * ROOM_HEIGHT + ROOM_HEIGHT / 2 };
}

function loadRoom(engine, level, room) {
  const ram = engine.ramView();
  ram[RAM_LEVEL] = level;
  ram[RAM_ROOM] = room;
  ram[RAM_MODE] = MODE_LEVEL_LOAD;
  engine.commitMemory();
  for (let frame = 0; frame < 600; frame++) {
    engine.run(0, 1, false);
    if (ram[RAM_MODE] === 3 || ram[RAM_MODE] === 4) {
      ram[RAM_ROOM] = room;
      engine.commitMemory();
    }
    if (ram[RAM_MODE] === MODE_PLAY && frame > 30) break;
  }
  for (let frame = 0; frame < 4; frame++) engine.run(0, 1, false);
}

function hideObjects(engine) {
  const ram = engine.ramView();
  for (let i = RAM_OAM; i < RAM_OAM + 0x100; i += 4) ram[i] = 0xf8;
  engine.commitMemory();
}

// The level block marks the rooms of a dungeon with one bit per grid cell, row by row,
// after rotating its columns by the rotation byte. Reading the ROM keeps this independent of
// what the game last copied into cartridge RAM.
function occupancy(engine, level) {
  const info = ROM_HEADER + LEVEL_INFO_START + (level - 1) * LEVEL_INFO_SIZE,
    rotation = engine.rom[info + LEVEL_INFO_ROTATION] & (COLUMNS - 1),
    mask = engine.rom.subarray(info + LEVEL_INFO_MASK, info + LEVEL_INFO_MASK + COLUMNS);
  return (room) => (mask[((room & (COLUMNS - 1)) + rotation) & (COLUMNS - 1)] & (0x80 >> (room >> 4))) !== 0;
}

export function buildMap(engine, level) {
  const saved = engine.capture();
  engine.boot(0);
  if (level !== 0) loadRoom(engine, level, ENTRANCES[level]);
  const used = level === 0 ? () => true : occupancy(engine, level);
  const start = engine.capture();
  const tiles = [];
  for (let room = 0; room < COLUMNS * ROWS; room++) {
    if (!used(room)) continue;
    engine.restore(start);
    loadRoom(engine, level, room);
    hideObjects(engine);
    engine.run(0, 1, true);
    const frame = engine.pixels().data,
      [column, row] = cellOf(room);
    tiles.push({
      x: column * ROOM_WIDTH,
      y: row * ROOM_HEIGHT,
      width: ROOM_WIDTH,
      height: ROOM_HEIGHT,
      data: frame.slice(PLAY_TOP * FRAME_WIDTH * 4, (PLAY_TOP + ROOM_HEIGHT) * FRAME_WIDTH * 4),
    });
  }
  engine.restore(saved);
  return { ...MAP_SIZES[level], tiles, reached: tiles.length };
}

// In the frame, Link's top row is object Y - 5: the OAM row is object Y + 2, the first drawn
// scanline is one below the OAM row, and the frame skips the first 8 scanlines.
export function buildSprites(engine) {
  const saved = engine.capture();
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture(),
    x = ram[RAM_OBJECT_X],
    y = ram[RAM_OBJECT_Y] - 5,
    facesLeft = (ram[0x98] & 2) !== 0;
  engine.run(0, 1, true);
  const shown = engine.pixels().data;
  engine.restore(start);
  hideObjects(engine);
  engine.run(0, 1, true);
  const hidden = engine.pixels().data;
  const size = 48,
    sheet = new Uint8ClampedArray(size * size * 2 * 4),
    origin = (size - LINK_SIZE) / 2,
    own = facesLeft ? 1 : 0,
    other = 1 - own,
    index = (half, column, row) => ((half * size + origin + row) * size + origin + column) * 4;
  for (let row = 0; row < LINK_SIZE; row++)
    for (let column = 0; column < LINK_SIZE; column++) {
      const px = x + column, py = y + row;
      if (px < 0 || px >= FRAME_WIDTH || py < 0 || py >= FRAME_HEIGHT) continue;
      const s = (py * FRAME_WIDTH + px) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      sheet.set(shown.subarray(s, s + 4), index(own, column, row));
      sheet.set(shown.subarray(s, s + 4), index(other, LINK_SIZE - 1 - column, row));
    }
  engine.restore(saved);
  return { width: size, height: size * 2, data: sheet };
}

export default {
  id: "zelda",
  title: "The Legend of Zelda",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 131088,
    sha256: "44e33dc0a6fb663b98908ea761b33bbd77ca5092f25c624b10bf2a97e5406a5a",
  },
  ramBytes: 0x2800,
  cartridgeMemory: true,
  levelCount: LEVELS,
  levelsCleared: (observation) => (observation.cleared ? Math.min(observation.cleared + 1, LEVELS) : 0),
  grid: true,
  calmCamera: true,
  exactCellRows: true,
  route: routeOrder,
  entry,
  pace: 300,
  maxActionFrames: 255,
  menuButtons: true,
  system: "NES",
  assets: "",
  screen: { width: FRAME_WIDTH, height: FRAME_HEIGHT },
  closeUp: { width: ROOM_WIDTH * 3, height: ROOM_HEIGHT * 3 },
  sprite: { width: 48, height: 48, originX: 16, originY: 16, icon: [16, 16, 16, 16] },
  controls: { a: "Sword", b: "Item", select: "Items" },
  campaign: true,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>The Legend of Zelda</h2><p>The Legend of Zelda is © Nintendo. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The overworld and dungeon maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: NAMES.map((name, id) => ({ id, name, label: name, ...MAP_SIZES[id] })),
    levels: NAMES.map((name, id) => ({ id, label: name, rooms: [id] })),
  },
  buildMap,
  buildSprites,
  decode,
  boot(engine, level) {
    engine.boot(0);
    if (level) loadRoom(engine, level, ENTRANCES[level]);
    return engine.capture();
  },
};
