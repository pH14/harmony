// SPDX-License-Identifier: AGPL-3.0-or-later
const LEVEL_COUNT = 32,
  WORLD = 0x75f,
  LEVEL = 0x75c,
  AREA_NUMBER = 0x760,
  AREA_TYPE = 0x74e,
  AREA_OFFSET = 0x74f,
  MODE = 0x770,
  TASK = 0x772,
  ENGINE = 0x0e,
  FLAG = 0x746,
  PLAY = 1,
  VICTORY = 2,
  PLAYING_TASK = 3,
  NORMAL = 8,
  KILLED = 0x0b,
  ADVANCED_FLAG = 5,
  START = 8,
  FLAGPOLE = 0x31,
  INTRO_WORLDS = [0, 1, 3, 6];

export const MAIN_AREAS = [
  37, 64, 38, 96, 40, 1, 39, 98, 36, 53, 32, 99, 34, 65, 44, 97,
  42, 49, 38, 98, 46, 35, 45, 96, 51, 1, 39, 100, 48, 50, 33, 101,
];
export const MAP_WIDTHS = [
  3378, 3072, 2622, 2278, 3168, 3078, 3793, 2278, 3410, 3555, 2611, 2278, 3812, 3072, 2548, 2795,
  3394, 3409, 2622, 2278, 3221, 3666, 2862, 2278, 3075, 3078, 3793, 3302, 6225, 3667, 3668, 4839,
];
export const MAP_HEIGHT = 224;
const HUD_ROWS = 24,
  SPRITE_TOP = 9,
  AREA_POINTER = 0x750,
  ENTRANCE_PAGE = 0x751,
  AREA_SLOTS = 128,
  AREA_MAX_WIDTH = 8192,
  AREA_NAMES = ["Underwater", "Overground", "Underground", "Castle"];

export function areaRoom(level, area) {
  return LEVEL_COUNT + level * AREA_SLOTS + area;
}
function roomLevel(id) {
  return id < LEVEL_COUNT ? id : Math.floor((id - LEVEL_COUNT) / AREA_SLOTS);
}
function roomArea(id) {
  return id < LEVEL_COUNT ? MAIN_AREAS[id] : (id - LEVEL_COUNT) % AREA_SLOTS;
}
export function areaMap(id) {
  if (!Number.isInteger(id) || id < LEVEL_COUNT || id >= areaRoom(LEVEL_COUNT, 0)) return null;
  const level = roomLevel(id), area = roomArea(id), name = AREA_NAMES[area >> 5];
  if (area === MAIN_AREAS[level]) return null;
  return { id, level, name, label: `${levelName(level)} ${name}`, height: MAP_HEIGHT };
}

export function levelName(id) {
  return `World ${(id >> 2) + 1}-${(id & 3) + 1}`;
}
function currentLevel(ram) {
  const level = ram[LEVEL];
  return ram[FLAG] === ADVANCED_FLAG ? Math.max(0, level - 1) : level;
}
function levelId(ram) {
  return ram[WORLD] * 4 + currentLevel(ram);
}
function room(ram) {
  return ram[AREA_TYPE] * 32 + (ram[AREA_OFFSET] & 31);
}
function dead(ram) {
  return ram[ENGINE] === KILLED || ram[0xb5] >= 2;
}
function victory(ram) {
  return ram[MODE] === VICTORY && ram[WORLD] === 7;
}
function castleCleared(ram) {
  return ram[MODE] === VICTORY && ram[WORLD] !== 7;
}
function onMap(ram) {
  return ram[MODE] === PLAY && ram[TASK] === PLAYING_TASK && room(ram) === MAIN_AREAS[levelId(ram)];
}
function inArea(ram) {
  return ram[MODE] === PLAY && ram[TASK] === PLAYING_TASK && room(ram) !== MAIN_AREAS[levelId(ram)];
}
function mapRoom(ram) {
  return inArea(ram) ? areaRoom(levelId(ram), room(ram)) : levelId(ram);
}
function playable(ram, id) {
  return onMap(ram) && ram[ENGINE] === NORMAL && ram[FLAG] === 0 && levelId(ram) === id;
}
function camera(ram) {
  return ram[0x71a] * 256 + ram[0x71c];
}
function playerX(ram) {
  return ram[0x6d] * 256 + ram[0x86];
}
function playerY(ram) {
  if (ram[0xb5] !== 1) return ram[0xb5] === 0 ? 0 : MAP_HEIGHT - 1;
  return Math.min(MAP_HEIGHT - 1, ram[0xce] + SPRITE_TOP);
}

export function decode(ram) {
  if (ram.length !== 2048) throw new Error("Invalid Super Mario Bros. memory");
  const id = levelId(ram),
    x = playerX(ram),
    loading = !(ram[MODE] === PLAY && ram[TASK] === PLAYING_TASK),
    screenX = loading ? 0 : Math.min(15, Math.max(0, x - camera(ram)) >> 4);
  return {
    x,
    y: playerY(ram),
    health: dead(ram) ? 0 : 1,
    level: mapRoom(ram),
    gamestate: ram[MODE],
    goal: ram[FLAG] !== 0,
    on_ground: ram[0x1d] === 0,
    facing: ram[0x33],
    loading,
    cleared: victory(ram) ? LEVEL_COUNT : id + (ram[FLAG] !== 0 || castleCleared(ram) ? 1 : 0),
    scene: room(ram),
    climb: (loading ? 0 : camera(ram) >> 4) + screenX,
    time: ram[0x7f8],
    fuel: ram[0x6d9] === ram[0x6da] ? 1 : 0,
    clock: ram[0x7f8] * 100 + ram[0x7f9] * 10 + ram[0x7fa],
  };
}

function idleUntil(engine, done, limit, render = false) {
  const ram = engine.ramView(),
    actions = [];
  for (let frames = 0; frames < limit; frames++) {
    if (done(ram)) return actions;
    engine.run(0, 1, render);
    const last = actions[actions.length - 1];
    if (last !== undefined && (last & 255) < 120) actions[actions.length - 1]++;
    else actions.push(1);
  }
  throw new Error("Super Mario Bros. did not reach the next level");
}

function boot(engine, id) {
  if (!Number.isInteger(id) || id < 0 || id >= LEVEL_COUNT) throw new Error("Unknown Super Mario Bros. starting level");
  const world = id >> 2,
    level = id & 3;
  engine.run(0, 120, true);
  const ram = engine.ramView();
  ram[WORLD] = world;
  ram[LEVEL] = level;
  ram[AREA_NUMBER] = level + (INTRO_WORLDS.includes(world) && level >= 2 ? 1 : 0);
  engine.run(START, 1, true);
  idleUntil(engine, (r) => playable(r, id), 900, true);
}

function hideSprites(ram) {
  ram[0x779] &= ~0x10;
  for (let i = 0x204; i < 0x300; i += 4) ram[i] = 0xf8;
}

const LANES = Array.from({ length: 19 }, (_, i) => 32 + ((i * 7) % 19) * 8);

function enterArea(engine, area) {
  const ram = engine.ramView();
  ram[AREA_POINTER] = area;
  ram[ENTRANCE_PAGE] = 0;
  ram[TASK] = 0;
  idleUntil(engine, (r) => r[TASK] === PLAYING_TASK && r[ENGINE] === NORMAL && room(r) === area, 900, true);
}

export function buildMap(engine, id, width = MAP_WIDTHS[id] ?? AREA_MAX_WIDTH) {
  const level = roomLevel(id), area = roomArea(id);
  engine.boot(level);
  if (id >= LEVEL_COUNT) enterArea(engine, area);
  const ram = engine.ramView();
  let data = new Uint8ClampedArray(width * MAP_HEIGHT * 4);
  const lanes = [ram[0xce], ...LANES];
  let farthest = -1,
    ahead = playerX(ram),
    still = 0,
    lane = 0;
  for (let frame = 0; frame < 8000 && still < 400; frame++) {
    const flying = ram[FLAG] === 0 && ram[ENGINE] === NORMAL;
    if (flying) {
      ram[0xce] = lanes[lane];
      ram[0xb5] = 1;
      ram[0x9f] = 0;
      ram[0x79f] = 30;
      ram[0x7f8] = 4;
      ram[0x745] = 0;
      for (let slot = 0; slot < 5; slot++) if (ram[0x16 + slot] !== FLAGPOLE) ram[0x0f + slot] = 0;
    }
    hideSprites(ram);
    engine.run(flying ? 0x82 : 0, 1, true);
    if (ram[MODE] !== PLAY || levelId(ram) !== level || (ram[TASK] === PLAYING_TASK && room(ram) !== area)) break;
    const left = camera(ram);
    if (id >= LEVEL_COUNT && farthest >= 0 && left > farthest + 64) break;
    if (frame > 8 && left > farthest) {
      const pixels = engine.pixelView(),
        from = farthest < 0 ? 0 : 160;
      for (let y = HUD_ROWS; y < MAP_HEIGHT; y++) {
        const end = Math.min(256, width - left);
        if (end > from) data.set(pixels.subarray((y * 256 + from) * 4, (y * 256 + end) * 4), (y * width + left + from) * 4);
      }
      if (farthest < 0) {
        const counts = new Map(),
          colors = new Uint32Array(pixels.buffer, pixels.byteOffset, 256 * MAP_HEIGHT);
        for (let i = HUD_ROWS * 256; i < colors.length; i++) counts.set(colors[i], (counts.get(colors[i]) || 0) + 1);
        const backdrop = [...counts].reduce((a, b) => (b[1] > a[1] ? b : a))[0];
        new Uint32Array(data.buffer, 0, HUD_ROWS * width).fill(backdrop);
      }
      farthest = left;
    }
    if (playerX(ram) > ahead) {
      ahead = playerX(ram);
      still = 0;
    } else if (flying && ++still % 12 === 0) lane = (lane + 1) % lanes.length;
  }
  const reached = farthest + 256;
  if (id >= LEVEL_COUNT && reached < width) {
    const cropped = new Uint8ClampedArray(reached * MAP_HEIGHT * 4);
    for (let y = 0; y < MAP_HEIGHT; y++) cropped.set(data.subarray(y * width * 4, (y * width + reached) * 4), y * reached * 4);
    data = cropped;
    width = reached;
  }
  return { width, height: MAP_HEIGHT, data, reached };
}

export function buildSprites(engine) {
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = playerX(ram) - camera(ram),
    sy = playerY(ram);
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    sheet = new Uint8ClampedArray(16 * 32 * 4);
  for (let y = 0; y < 16; y++)
    for (let x = 0; x < 16; x++) {
      const s = ((sy + y) * 256 + sx + x) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      sheet.set(shown.subarray(s, s + 4), (y * 16 + x) * 4);
      sheet.set(shown.subarray(s, s + 4), ((16 + y) * 16 + 15 - x) * 4);
    }
  engine.restore(start);
  return { width: 16, height: 32, data: sheet };
}

export default {
  id: "super-mario-bros",
  title: "Super Mario Bros.",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 40976,
    sha256: "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  pace: 120,
  maxActionFrames: 120,
  menuButtons: false,
  system: "NES",
  assets: "",
  screen: { width: 256, height: 224 },
  sprite: { width: 16, height: 16, originX: 0, originY: 0, icon: [0, 0, 16, 16] },
  controls: { a: "Jump", b: "Run" },
  lede: `Watch Harmony play Super Mario Bros. right in your browser, from a ROM you supply.`,
  attribution: `Super Mario Bros. runs from the ROM you supplied. It stays in this browser.`,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Super Mario Bros.</h2><p>Super Mario Bros. is © Nintendo. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The level maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, name: levelName(id), label: levelName(id), width: MAP_WIDTHS[id], height: MAP_HEIGHT })),
    levels: Array.from({ length: LEVEL_COUNT }, (_, id) => ({ id, label: levelName(id), rooms: [id] })),
  },
  buildMap,
  buildSprites,
  areaMap,
  decode,
  ended: (ram) => dead(ram) || ram[FLAG] !== 0 || ram[MODE] !== PLAY,
  levelComplete: (ram) => !dead(ram) && (ram[FLAG] !== 0 || castleCleared(ram)),
  transition(engine) {
    const id = levelId(engine.ramView()) + 1;
    return idleUntil(engine, (ram) => playable(ram, id), 3000);
  },
  motion(ram) {
    const valid = ram[MODE] === PLAY && ram[TASK] === PLAYING_TASK && !dead(ram);
    return [valid ? mapRoom(ram) : 65535, playerX(ram), playerY(ram), ram[0x33] === 2 ? 1 : 0];
  },
  boot,
};
