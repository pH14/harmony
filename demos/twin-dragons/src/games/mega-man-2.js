// SPDX-License-Identifier: AGPL-3.0-or-later
const STAGE = 0x2a,
  PLAYER_STATE = 0x2c,
  WEAPONS = 0x9a,
  ITEMS = 0x9b,
  ENERGY = 0x9c,
  SCREEN = 0x440,
  ROOM = 0x38,
  CAMERA_SCREEN = 0x20,
  CAMERA_FINE = 0x1f,
  CAMERA_STATE = 0x1b,
  BANK = 0x29,
  MENU_MODE = 0xf7,
  CURRENT_BOSS = 0xb3,
  REFIGHTS = 0xbc,
  PLAYER_X = 0x460,
  PLAYER_Y = 0x4a0,
  PLAYER_FLAGS = 0x420,
  HEALTH = 0x6c0,
  BOSS_HEALTH = 0x6c1,
  BOSS_PHASE = 0xb1,
  GAMEPLAY = 0x0e,
  STAGE_SELECT = 0x90,
  STANDING = 3,
  AIRBORNE = 6,
  LADDER = 9,
  LADDER_TOP = 10,
  FALLEN = 1,
  DYING = 0,
  FULL = 28,
  DEFEATED = 0xfe,
  CLEARED = 0xff,
  WILY5 = 12,
  FINAL = 13,
  WILY_MACHINE = 12,
  A = 1,
  START = 8,
  UP = 0x10,
  DOWN = 0x20,
  LEFT = 0x40,
  RIGHT = 0x80,
  SCREEN_HEIGHT = 224,
  CROP_TOP = 8;

export const ORDER = [7, 5, 6, 1, 3, 0, 2, 4, 8, 9, 10, 11, 12, 13];
const NAMES = ["Heat Man", "Air Man", "Wood Man", "Bubble Man", "Quick Man", "Flash Man", "Metal Man", "Crash Man", "Wily 1", "Wily 2", "Wily 3", "Wily 4", "Wily 5", "Wily 6"];
const SELECT_PATHS = [[LEFT], [UP], [RIGHT], [UP, LEFT], [UP, RIGHT], [DOWN], [DOWN, LEFT], [DOWN, RIGHT]];
const LEVEL_COUNT = ORDER.length;

export const STAGES = [
  [0, 0, [0x47, 0x40, 0x4a, 0x40, 0x20, 0x20, 0x00]],
  [0, 0, [0x49, 0x40, 0x28, 0x20, 0x00]],
  [0, 0, [0x44, 0x40, 0x20, 0x20, 0x20, 0x80, 0x80, 0x44, 0x40, 0x40, 0x40, 0x22, 0x20, 0x00]],
  [0, 0, [0x44, 0x40, 0x40, 0x40, 0x26, 0x24, 0x20, 0x00]],
  [0, 0, [0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x22, 0x20, 0x00]],
  [0, 0, [0x46, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x24, 0x20, 0x00]],
  [0, 0, [0x49, 0x40, 0x28, 0x20, 0x00]],
  [0, 0, [0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x81, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x21, 0x20, 0x00]],
  [7, 24, [0x88, 0x80, 0x80, 0x80, 0x80, 0x80, 0x04]],
  [5, 22, [0x45, 0x40, 0x40, 0x40, 0x45, 0x40, 0x40, 0x00]],
  [14, 24, [0x40, 0x40, 0x40, 0x44, 0x40, 0x40, 0x40, 0x40, 0x22, 0x00]],
  [8, 22, [0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x41, 0x40, 0x40, 0x40, 0x40, 0x23, 0x00]],
  [18, 24, [0x00]],
  [10, 22, [0x40, 0x40, 0x40, 0x25, 0x00]],
];

export function layout(stage) {
  const [room, start, entries] = STAGES[stage];
  const placed = [];
  let col = 0, row = 0, screen = start;
  const rooms = entries.map((entry, i) => {
    const count = (entry & 31) + 1,
      info = { room: room + i, first: screen, last: screen + count - 1, col, row, exit: entry & 0xe0 };
    for (let k = 0; k < count; k++) placed.push([screen + k, col + k, row]);
    screen += count;
    col += count - 1;
    if (entry & 0x40) row++;
    else if (entry & 0x80) row--;
    else if (entry & 0x20) col++;
    return info;
  });
  const top = Math.min(...placed.map((p) => p[2]));
  for (const info of rooms) info.row -= top;
  return {
    start,
    cells: new Map(placed.map(([s, c, r]) => [s, [c, r - top]])),
    rooms,
    cols: Math.max(...placed.map((p) => p[1])) + 1,
    rows: Math.max(...placed.map((p) => p[2])) - top + 1,
  };
}
const LAYOUTS = Array.from({ length: 14 }, (_, stage) => layout(stage));
export const MAP_SIZES = ORDER.map((stage) => ({ width: LAYOUTS[stage].cols * 256, height: LAYOUTS[stage].rows * SCREEN_HEIGHT }));
const ROUTES = ORDER.map((stage) => {
  const lay = LAYOUTS[stage];
  return new Map([...lay.cells].map(([screen, [col, row]]) => [`${col},${row}`, screen - lay.start]));
});
export function routeOrder(level, x, y) {
  const index = ROUTES[level]?.get(`${Math.floor(x / 256)},${Math.floor(y / SCREEN_HEIGHT)}`);
  return index === undefined ? -1 : index * 256 + (x & 255);
}
export function entry(level) {
  const lay = LAYOUTS[ORDER[level]], [col, row] = lay.cells.get(lay.start);
  return { x: col * 256 + 64, y: row * SCREEN_HEIGHT + SCREEN_HEIGHT / 2 };
}

export function levelName(id) {
  return NAMES[ORDER[id]];
}
function levelOf(stage) {
  return Math.max(0, ORDER.indexOf(stage));
}
function cell(ram) {
  const lay = LAYOUTS[ram[STAGE]] || LAYOUTS[0];
  return lay.cells.get(ram[SCREEN]) || lay.cells.get(lay.start);
}
function playerX(ram) {
  return cell(ram)[0] * 256 + ram[PLAYER_X];
}
function playerY(ram) {
  return Math.min(cell(ram)[1] * SCREEN_HEIGHT + SCREEN_HEIGHT - 1, cell(ram)[1] * SCREEN_HEIGHT + Math.max(0, ram[PLAYER_Y] - CROP_TOP));
}
function dead(ram) {
  return ram[HEALTH] === 0 || ram[PLAYER_STATE] === DYING || (ram[PLAYER_STATE] === FALLEN && ram[PLAYER_Y] >= 0xe0);
}
function inPlay(ram) {
  return ram[BANK] === GAMEPLAY && ram[STAGE] < 14 && ram[PLAYER_STATE] >= 2 && ram[PLAYER_STATE] <= LADDER_TOP;
}
function lastCastleBoss(ram) {
  return ram[STAGE] !== WILY5 || (ram[REFIGHTS] === 0xff && ram[CURRENT_BOSS] === WILY_MACHINE);
}
function complete(ram) {
  if (ram[BANK] !== GAMEPLAY || ram[STAGE] >= 14) return false;
  if (ram[STAGE] < 8) return ram[BOSS_PHASE] >= DEFEATED;
  return ram[BOSS_PHASE] === CLEARED && lastCastleBoss(ram);
}
function won(ram) {
  return ram[STAGE] === FINAL && complete(ram);
}
function bossDamage(ram) {
  if (ram[BOSS_PHASE] >= DEFEATED) return FULL;
  return ram[BOSS_PHASE] >= 2 && ram[BOSS_HEALTH] > 0 ? Math.max(0, FULL - ram[BOSS_HEALTH]) : 0;
}

export function decode(ram) {
  if (ram.length !== 2048) throw new Error("Invalid Mega Man 2 memory");
  const level = levelOf(ram[STAGE]),
    lay = LAYOUTS[ram[STAGE]] || LAYOUTS[0],
    loading = !inPlay(ram),
    done = complete(ram);
  return {
    x: playerX(ram),
    y: playerY(ram),
    health: dead(ram) ? 0 : ram[HEALTH],
    level,
    gamestate: ram[BOSS_PHASE],
    goal: done,
    on_ground: ram[PLAYER_STATE] === STANDING,
    facing: ram[PLAYER_FLAGS] & 0x40 ? 1 : 0,
    loading,
    cleared: won(ram) ? LEVEL_COUNT : level + (done ? 1 : 0),
    scene: ram[ROOM],
    climb: Math.max(0, ram[SCREEN] - lay.start) * 256 + ram[PLAYER_X],
    time: bossDamage(ram),
    fuel: 0,
    clock: 0,
  };
}

function record(actions, buttons, frames) {
  const last = actions[actions.length - 1];
  if (last !== undefined && last >> 8 === buttons && (last & 255) + frames <= 255) actions[actions.length - 1] += frames;
  else actions.push((buttons << 8) | frames);
}
function driver(engine, render) {
  const actions = [];
  return {
    actions,
    run(buttons, frames) {
      for (let left = frames; left > 0; left -= 255) {
        const n = Math.min(255, left);
        if (render) engine.run(buttons, n, true);
        else engine.step(buttons, n);
        record(actions, buttons, n);
      }
    },
    until(done, limit) {
      const ram = engine.ramView();
      for (let frames = 0; frames < limit; frames++) {
        if (done(ram)) return;
        this.run(0, 1);
      }
      throw new Error("Mega Man 2 did not reach the next stage");
    },
  };
}
function arrived(ram, stage) {
  return ram[STAGE] === stage && ram[BANK] === GAMEPLAY && ram[PLAYER_STATE] === STANDING && ram[HEALTH] === FULL;
}
function select(drive, stage) {
  for (const direction of SELECT_PATHS[stage] || []) {
    drive.run(direction, 4);
    drive.run(0, 30);
  }
  drive.run(START, 4);
}
function award(ram, beaten) {
  ram[WEAPONS] = beaten;
  ram[ITEMS] = (beaten & 1 ? 1 : 0) | (beaten & 2 ? 2 : 0) | (beaten & 0x20 ? 4 : 0);
  for (let i = 0; i < 8; i++) ram[ENERGY + i] = beaten & (1 << i) ? FULL : 0;
  for (let i = 0; i < 3; i++) ram[ENERGY + 8 + i] = ram[ITEMS] & (1 << i) ? FULL : 0;
}

function boot(engine, id) {
  if (!Number.isInteger(id) || id < 0 || id >= LEVEL_COUNT) throw new Error("Unknown Mega Man 2 starting stage");
  const drive = driver(engine, true),
    ram = engine.ramView(),
    stage = ORDER[id];
  drive.run(0, 600);
  for (let i = 0; i < 3; i++) {
    drive.run(START, 4);
    drive.run(0, 300);
  }
  if (stage < 8) {
    award(ram, ORDER.slice(0, id).reduce((bits, s) => bits | (1 << s), 0));
    select(drive, stage);
    drive.until((r) => arrived(r, stage), 1200);
    return;
  }
  award(ram, 0);
  select(drive, 0);
  for (let frames = 0; frames < 3000 && !arrived(ram, stage); frames++) {
    ram[STAGE] = stage;
    drive.run(0, 1);
  }
  if (!arrived(ram, stage)) throw new Error("Mega Man 2 did not reach the castle");
  award(ram, 0xff);
}

function transition(engine) {
  const drive = driver(engine, false),
    ram = engine.ramView(),
    stage = ram[STAGE];
  if (stage >= 8) {
    drive.until((r) => arrived(r, stage + 1), 8000);
    return drive.actions;
  }
  drive.until((r) => r[WEAPONS] & (1 << stage), 1800);
  drive.run(0, 900);
  for (let round = 0; round < 16 && ram[MENU_MODE] !== STAGE_SELECT; round++) {
    drive.run(DOWN, 4);
    drive.run(0, 30);
    drive.run(START, 4);
    drive.run(0, 300);
  }
  if (ram[MENU_MODE] !== STAGE_SELECT) throw new Error("Mega Man 2 did not return to stage select");
  const next = ORDER.find((s) => s < 8 && !(ram[WEAPONS] & (1 << s))) ?? 8;
  select(drive, next);
  drive.until((r) => arrived(r, next), next < 8 ? 1200 : 8000);
  return drive.actions;
}

const LANES = [0x30, 0x60, 0x90, 0xb0, 0x20, 0x48, 0x78, 0xa0, 0xc0];

export function buildMap(engine, id) {
  engine.boot(id);
  const stage = ORDER[id],
    lay = LAYOUTS[stage],
    ram = engine.ramView(),
    width = lay.cols * 256,
    height = lay.rows * SCREEN_HEIGHT,
    data = new Uint8ClampedArray(width * height * 4);
  let room = 0;
  const clearEnemies = () => {
    for (let i = 0x10; i < 0x20; i++) ram[PLAYER_FLAGS + i] = 0;
  };
  const copy = () => {
    const info = lay.rooms[room], at = lay.cells.get(ram[CAMERA_SCREEN]);
    if (!at || ram[ROOM] !== info.room || ram[CAMERA_STATE] !== 0 || ram[BANK] !== GAMEPLAY) return;
    const x0 = at[0] * 256 + ram[CAMERA_FINE], y0 = at[1] * SCREEN_HEIGHT,
      end = Math.min(256, (info.col + info.last - info.first + 1) * 256 - x0),
      pixels = engine.pixelView();
    if (end <= 0) return;
    for (let y = 0; y < SCREEN_HEIGHT; y++) data.set(pixels.subarray(y * 1024, y * 1024 + end * 4), ((y0 + y) * width + x0) * 4);
  };
  const step = (buttons) => {
    ram[HEALTH] = FULL;
    clearEnemies();
    for (let i = 0x200; i < 0x300; i += 4) ram[i] = 0xf8;
    engine.run(buttons, 1, true);
    copy();
  };
  const settle = (frames) => {
    for (let i = 0; i < frames; i++) step(0);
  };
  const advance = (x) => {
    ram[SCREEN] = x >> 8;
    ram[PLAYER_X] = x & 255;
    ram[PLAYER_STATE] = AIRBORNE;
  };
  const traverse = (info) => {
    let best = -1, still = 0, lane = 0, tried = 0, deaths = 0, good = engine.capture();
    for (let f = 0; f < 30000; f++) {
      if (ram[CAMERA_SCREEN] === info.last && ram[CAMERA_FINE] === 0 && ram[PLAYER_STATE] !== DYING) return true;
      if (ram[ROOM] !== info.room) return false;
      ram[PLAYER_Y] = LANES[lane];
      if (ram[PLAYER_STATE] === LADDER || ram[PLAYER_STATE] === LADDER_TOP) ram[PLAYER_STATE] = AIRBORNE;
      step(RIGHT);
      if (ram[PLAYER_STATE] === DYING || ram[PLAYER_STATE] === FALLEN) {
        engine.restore(good);
        lane = (lane + 1) % LANES.length;
        still = 0;
        if (++deaths % LANES.length === 0) advance(ram[SCREEN] * 256 + ram[PLAYER_X] + 16);
        continue;
      }
      if (f % 16 === 0) good = engine.capture();
      const x = ram[SCREEN] * 256 + ram[PLAYER_X];
      if (x > best) {
        best = x;
        still = tried = 0;
      } else if (++still > 30) {
        still = 0;
        lane = (lane + 1) % LANES.length;
        if (++tried >= LANES.length) {
          advance(x + 24);
          tried = 0;
        }
      }
    }
    return false;
  };
  const vertical = (info) => {
    const at = engine.capture(), key = info.exit & 0x40 ? DOWN : UP, want = info.room + 1;
    const attempt = (x, y, state, frames, land) => {
      engine.restore(at);
      ram[PLAYER_X] = x;
      ram[PLAYER_Y] = y;
      ram[SCREEN] = info.last;
      if (state >= 0) ram[PLAYER_STATE] = state;
      for (let f = 0, still = 0; f < frames; f++) {
        const before = ram[PLAYER_X];
        ram[HEALTH] = FULL;
        clearEnemies();
        engine.run(f < land ? 0 : key, 1, false);
        if (ram[PLAYER_STATE] === DYING) ram[PLAYER_STATE] = AIRBORNE;
        if (ram[ROOM] === want && ram[CAMERA_STATE] === 0) return true;
        if (state === LADDER && f > 30 && ram[PLAYER_STATE] !== LADDER && ram[PLAYER_STATE] !== LADDER_TOP) return false;
        still = f >= land && ram[PLAYER_STATE] === STANDING && ram[PLAYER_X] === before ? still + 1 : 0;
        if (still > 30) return false;
      }
      return false;
    };
    for (const y of [0x30, 0x90, 0xc8, 0x60, 0xb0, 0x18]) for (let x = 4; x < 256; x += 4) if (attempt(x, y, -1, 360, 40)) return true;
    for (let y = 0x10; y < 0xe8; y += 0x10) for (let x = 4; x < 256; x += 8) if (attempt(x, y, LADDER, 400, 0)) return true;
    engine.restore(at);
    return false;
  };
  const door = (info) => {
    const at = engine.capture(), want = info.room + 1;
    for (const key of [RIGHT, RIGHT | UP])
      for (let x = 0xe8; x >= 0x80; x -= 0x18)
        for (let y = 0x18; y < 0xe0; y += 0x10) {
          engine.restore(at);
          ram[PLAYER_X] = x;
          ram[PLAYER_Y] = y;
          ram[PLAYER_STATE] = AIRBORNE;
          for (let f = 0; f < 500; f++) {
            ram[HEALTH] = FULL;
            engine.run(key, 1, false);
            if (f === 120 && ram[CAMERA_STATE] === 0 && ram[SCREEN] === info.last && ram[PLAYER_X] < x + 4) break;
            if (ram[ROOM] === want) return true;
          }
        }
    engine.restore(at);
    return false;
  };
  settle(4);
  while (room < lay.rooms.length) {
    const info = lay.rooms[room];
    if (!traverse(info)) break;
    settle(2);
    if (!info.exit || !(info.exit & 0x20 ? door(info) : vertical(info))) break;
    for (let k = 0; k < 600 && ram[CAMERA_STATE] !== 0; k++) engine.run(0, 1, false);
    room++;
    settle(12);
  }
  return { width, height, data, reached: room };
}

export function buildSprites(engine) {
  engine.boot(levelOf(0));
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = ram[PLAYER_X] - ram[CAMERA_FINE],
    sy = ram[PLAYER_Y] - CROP_TOP;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    for (let i = 0x200; i < 0x300; i += 4) ram[i] = 0xf8;
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    size = 32,
    sheet = new Uint8ClampedArray(size * size * 2 * 4),
    facesRight = ram[PLAYER_FLAGS] & 0x40;
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size; x++) {
      const px = sx - 16 + x, py = sy - 16 + y;
      if (px < 0 || px >= 256 || py < 0 || py >= SCREEN_HEIGHT) continue;
      const s = (py * 256 + px) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      const right = (y * size + x) * 4, left = ((size + y) * size + size - 1 - x) * 4;
      sheet.set(shown.subarray(s, s + 4), facesRight ? right : left);
      sheet.set(shown.subarray(s, s + 4), facesRight ? left : right);
    }
  engine.restore(start);
  return { width: size, height: size * 2, data: sheet };
}

export default {
  id: "mega-man-2",
  title: "Mega Man 2",
  core: "quicknes",
  playthrough: { id: "RAVZgbFCTuU" },
  rom: {
    upload: true,
    bytes: 262160,
    sha256: "49136b412ff61beac6e40d0bbcd8691a39a50cd2744fdcdde3401eed53d71edf",
  },
  ramBytes: 2048,
  levelCount: LEVEL_COUNT,
  grid: true,
  route: routeOrder,
  entry,
  pace: 120,
  maxActionFrames: 255,
  menuButtons: true,
  system: "NES",
  assets: "",
  screen: { width: 256, height: 224 },
  closeUp: { width: 320, height: 240 },
  sprite: { width: 32, height: 32, originX: 8, originY: 0, icon: [4, 4, 24, 24] },
  controls: { a: "Jump", b: "Fire" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Mega Man 2</h2><p>Mega Man 2 is © Capcom. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The stage maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Capcom.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: ORDER.map((_, id) => ({ id, name: levelName(id), label: levelName(id), ...MAP_SIZES[id] })),
    levels: ORDER.map((_, id) => ({ id, label: levelName(id), rooms: [id] })),
  },
  buildMap,
  buildSprites,
  decode,
  ended: (ram) => dead(ram) || complete(ram) || ram[BANK] !== GAMEPLAY,
  levelComplete: (ram) => !dead(ram) && complete(ram) && !won(ram),
  transition,
  motion(ram) {
    const valid = inPlay(ram) && !dead(ram);
    return [valid ? levelOf(ram[STAGE]) : 65535, playerX(ram), playerY(ram), ram[PLAYER_FLAGS] & 0x40 ? 0 : 1];
  },
  boot,
};
