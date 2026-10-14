// SPDX-License-Identifier: AGPL-3.0-or-later
const MODE = 0x1e,
  PLAYING = 3,
  SCROLL_DIRECTION = 0x49,
  FACING = 0x4d,
  MAP_Y = 0x4f,
  MAP_X = 0x50,
  DOOR = 0x56,
  AREA = 0x74,
  SCROLL_Y = 0xfc,
  SCROLL_X = 0xfd,
  PPU_CONTROL = 0xff,
  HEALTH_LOW = 0x106,
  HEALTH_HIGH = 0x107,
  POSE = 0x300,
  SAMUS_TABLE = 0x30c,
  SAMUS_Y = 0x30d,
  SAMUS_X = 0x30e,
  CARTRIDGE = 0x800 - 0x6000,
  TANKS = 0x6877 + CARTRIDGE,
  EQUIPMENT = 0x6878 + CARTRIDGE,
  MISSILES = 0x6879 + CARTRIDGE,
  KRAID = 0x687b + CARTRIDGE,
  RIDLEY = 0x687c + CARTRIDGE,
  ENDING = 0x6883 + CARTRIDGE,
  START = 8,
  SCREEN_WIDTH = 256,
  SCREEN_HEIGHT = 240;

const OWNERS =
  "...........................................b.b......b..............bbbbbbbbb.bbbbbbbb.........b..t.b.......b.bbbbbbbbbbbbbbbbbb..t.t.......b.b................b..t.t..bbbbbb.b..........bbbbbbb..t.t.......b.b....b...b.......b..t.ttttttttbbbbbbbbbbbbbbbbbbbb..t........tb.b....b...b.......b..t........tb.b....bbbbb.n.....b..t........tb.b....b...b.nnnnnnn..ttttttttttb.bbbbbbbbbb.nnnnnnn............b.b........b.nnnnnnn............b.b........n.n.....n..bbbbbbbbbbb.nnnnnnnnnnnnnnnnnn.......b....b.nnnnnnnnnnnnnnnnnn.......b.k..b.nnnnnn...........n.......b.kkkk.nnnnnnnnnnnnnnnnnn.......bbkkkk.nnnnnnnnnnnnnnnnnn.......bbkkkk.n...nn......nnnnnn..k.....kk..k.nnnnnnnnnnnnnnnnnn..kkkkkkkkkkk.n...nnnnn..nnnnnnn..kkkkkkkkkkk.n..nnnnnn..nn....n..k..k..k....k....n.....rnn....n..k..kkkkkkkkk.r..nrrrrrrnr....n..k..k..k.kkkk.rrrrrrrrrrrrrrrrr..k..kkkkkk.kk.r..r....r.......r..kkkkkkkkkkkk.rrrrrrrrrrrrrrrrr..k.kkkkkkkkkk.r.......r.......r.........kkkkk.rrrrrrrrrrrrrrrrr.............k.rrrrrrrrrrrrrrrrr.................................";

const AREAS = [
  { name: "Brinstar", letter: "b", bank: 1, gfx: [3, 4, 5, 6, 25, 22] },
  { name: "Norfair", letter: "n", bank: 2, gfx: [4, 5, 7, 8, 9, 25, 22] },
  { name: "Kraid's Lair", letter: "k", bank: 4, gfx: [4, 5, 10, 15, 16, 17, 25, 22] },
  { name: "Ridley's Lair", letter: "r", bank: 5, gfx: [4, 5, 10, 18, 19, 25, 22] },
  { name: "Tourian", letter: "t", bank: 3, gfx: [5, 10, 11, 12, 13, 14, 26, 28, 25, 22] },
];
const AREA_OF = [0, 1, 2, 4, 3];
for (const area of AREAS) {
  area.cells = [];
  for (let i = 0; i < 1024; i++) if (OWNERS[i] === area.letter) area.cells.push([i & 31, i >> 5]);
  area.x0 = Math.min(...area.cells.map((c) => c[0]));
  area.y0 = Math.min(...area.cells.map((c) => c[1]));
  area.cols = Math.max(...area.cells.map((c) => c[0])) - area.x0 + 1;
  area.rows = Math.max(...area.cells.map((c) => c[1])) - area.y0 + 1;
}
export const MAP_SIZES = AREAS.map((a) => ({ width: a.cols * SCREEN_WIDTH, height: a.rows * SCREEN_HEIGHT }));
export const AREA_ORIGINS = AREAS.map((a) => [a.x0, a.y0]);

function distances(area, start) {
  const owned = new Set(area.cells.map(([x, y]) => y * 32 + x)), far = new Map([[start, 0]]), queue = [start];
  while (queue.length) {
    const at = queue.shift();
    for (const next of [at - 1, at + 1, at - 32, at + 32])
      if (owned.has(next) && !far.has(next) && Math.abs((next & 31) - (at & 31)) <= 1) {
        far.set(next, far.get(at) + 1);
        queue.push(next);
      }
  }
  return far;
}
const STARTS = [[3, 14], [22, 13], [7, 20], [25, 24], [3, 4]];
const ROUTES = AREAS.map((area, id) => distances(area, STARTS[id][1] * 32 + STARTS[id][0]));

export function routeOrder(level, x, y) {
  const area = AREAS[level];
  if (!area) return -1;
  const cx = area.x0 + Math.floor(x / SCREEN_WIDTH), cy = area.y0 + Math.floor(y / SCREEN_HEIGHT);
  const steps = ROUTES[level].get(cy * 32 + cx);
  return steps === undefined ? -1 : steps * 256 + ((x & 255) + (y % SCREEN_HEIGHT)) / 2;
}
export function entry(level) {
  const area = AREAS[level], [cx, cy] = STARTS[level];
  return { x: (cx - area.x0) * SCREEN_WIDTH + 128, y: (cy - area.y0) * SCREEN_HEIGHT + 120 };
}

function bcd(byte) {
  return (byte >> 4) * 10 + (byte & 15);
}
function bits(byte) {
  let n = 0;
  for (; byte; byte &= byte - 1) n++;
  return n;
}
function samusCell(ram) {
  const direction = ram[SCROLL_DIRECTION], vertical = direction < 2, backward = direction === 0 || direction === 2;
  const scroll = ram[vertical ? SCROLL_Y : SCROLL_X];
  const inSelected = ram[SAMUS_TABLE] === (ram[PPU_CONTROL] & 3 ? 1 : 0);
  const ahead = scroll === 0 || inSelected === backward;
  const step = (v) => (ahead ? v : backward ? v + 1 : v - 1) & 0xff;
  return vertical ? [ram[MAP_X], step(ram[MAP_Y])] : [step(ram[MAP_X]), ram[MAP_Y]];
}
function areaOf(ram) {
  return AREA_OF[ram[AREA] & 15] ?? 0;
}
function healthTenths(ram) {
  return bcd(ram[HEALTH_HIGH]) * 100 + bcd(ram[HEALTH_LOW]);
}
function inPlay(ram) {
  return ram[MODE] === PLAYING;
}
function dead(ram) {
  const health = healthTenths(ram);
  return inPlay(ram) && (health === 0 || health >= 8000);
}
function won(ram) {
  return ram[ENDING] !== 0;
}
function mapPoint(ram) {
  const level = areaOf(ram), area = AREAS[level], [cx, cy] = samusCell(ram);
  const x = (cx - area.x0) * SCREEN_WIDTH + ram[SAMUS_X], y = (cy - area.y0) * SCREEN_HEIGHT + ram[SAMUS_Y];
  return [level, Math.max(0, Math.min(MAP_SIZES[level].width - 1, x)), Math.max(0, Math.min(MAP_SIZES[level].height - 1, y))];
}

export function decode(ram) {
  const [level, x, y] = mapPoint(ram), health = healthTenths(ram), door = ram[DOOR];
  const cleared = bits(ram[EQUIPMENT]) + Math.min(6, ram[TANKS]) + (ram[KRAID] >> 7) + (ram[RIDLEY] >> 7) + (won(ram) ? 1 : 0);
  return {
    x,
    y,
    health: dead(ram) ? 0 : Math.min(255, Math.ceil(health / 10)),
    level,
    next_level: 0,
    gamestate: ram[MODE],
    goal: won(ram),
    on_ground: ram[POSE] <= 1 || ram[POSE] === 0xff,
    facing: ram[FACING] ? 0 : 1,
    loading: !inPlay(ram) || (door !== 0 && door !== 5),
    cleared,
    scene: ram[AREA],
    climb: 0,
    time: 0,
    fuel: ram[MISSILES],
    clock: health,
  };
}

function offset(bank, address) {
  return address >= 0xc000 ? 0x10 + 7 * 0x4000 + address - 0xc000 : 0x10 + bank * 0x4000 + address - 0x8000;
}
function reader(rom, bank) {
  const byte = (address) => rom[offset(bank, address)];
  return { byte, word: (address) => byte(address) | (byte(address + 1) << 8) };
}
const NES_RGB = [
  0x545454, 0x001e74, 0x081090, 0x300088, 0x440064, 0x5c0030, 0x540400, 0x3c1800, 0x202a00, 0x083a00, 0x004000, 0x003c00, 0x00323c, 0, 0, 0,
  0x989698, 0x084cc4, 0x3032ec, 0x5c1ee4, 0x8814b0, 0xa01464, 0x982220, 0x783c00, 0x545a00, 0x287200, 0x087c00, 0x007628, 0x006678, 0, 0, 0,
  0xeceeec, 0x4c9aec, 0x787cec, 0xb062ec, 0xe454ec, 0xec58b4, 0xec6a64, 0xd48820, 0xa0aa00, 0x74c400, 0x4cd020, 0x38cc6c, 0x38b4cc, 0x3c3c3c, 0, 0,
  0xeceeec, 0xa8ccec, 0xbcbcec, 0xd4b2ec, 0xecaeec, 0xecaed4, 0xecb4b0, 0xe4c490, 0xccd278, 0xb4de78, 0xa8e290, 0x98e2b4, 0xa0d6e4, 0xa0a2a0, 0, 0,
];

function patterns(rom, entries) {
  const chr = new Uint8Array(0x2000), fixed = reader(rom, 7);
  for (const index of entries) {
    const at = 0xc6e0 + 7 * index, bank = fixed.byte(at), source = fixed.word(at + 1), target = fixed.word(at + 3), length = fixed.word(at + 5);
    const start = offset(bank, source);
    chr.set(rom.subarray(start, start + length), target);
  }
  return chr;
}
function palette(rom, bank) {
  const { byte, word } = reader(rom, bank), colors = new Uint8Array(32).fill(0x0f);
  let p = word(0x9560);
  while (byte(p)) {
    const address = (byte(p) << 8) | byte(p + 1), length = byte(p + 2), count = length & 0x3f || 0x40, stride = length & 0x80 ? 32 : 1;
    p += 3;
    for (let k = 0; k < count; k++) {
      const value = length & 0x40 ? byte(p) : byte(p++);
      const target = address + k * stride - 0x3f00;
      if (target >= 0 && target < 32) colors[target] = value;
    }
    if (length & 0x40) p++;
  }
  return colors;
}
function roomTiles(rom, bank, room) {
  const { byte, word } = reader(rom, bank), structures = word(0x959c), macros = word(0x959e);
  let p = word(word(0x959a) + 2 * room);
  const roomPalette = byte(p++) & 3, tiles = new Uint8Array(0x3c0).fill(0xff), attributes = new Uint8Array(0x40).fill([0, 0x55, 0xaa, 0xff][roomPalette]);
  const doors = [];
  for (;;) {
    const b = byte(p);
    if (b === 0xff) return { tiles, attributes, doors };
    if (b === 0xfe) {
      p++;
      continue;
    }
    if (b === 0xfd) {
      p++;
      break;
    }
    const objectPalette = byte(p + 2) & 3;
    let s = word(structures + 2 * byte(p + 1)), base = (b >> 4) * 0x40 + (b & 15) * 2;
    p += 3;
    for (let header = byte(s); header !== 0xff; header = byte(s)) {
      const count = header & 15 || 16;
      let at = base + (header >> 4) * 2, i = 0;
      while (i < count && at < 0x3c0) {
        const m = macros + 4 * byte(s + 1 + i);
        tiles[at] = byte(m);
        tiles[at + 1] = byte(m + 1);
        if (at + 0x20 < 0x3c0) tiles[at + 0x20] = byte(m + 2);
        if (at + 0x21 < 0x3c0) tiles[at + 0x21] = byte(m + 3);
        if (objectPalette !== roomPalette) {
          const row = at >> 5, col = at & 31, cell = (row >> 2) * 8 + (col >> 2), shift = 2 * ((row & 2 ? 2 : 0) + (col & 2 ? 1 : 0));
          attributes[cell] = (attributes[cell] & ~(3 << shift)) | (objectPalette << shift);
        }
        i++;
        at += 2;
        if ((at & 31) === 0) break;
      }
      if (at >= 0x3c0 && i < count) break;
      s += 1 + count;
      base += 0x40;
    }
  }
  const lengths = { 1: 3, 2: 2, 4: 2, 6: 1, 7: 3 };
  for (let b = byte(p); b !== 0xff && lengths[b & 15]; b = byte(p)) {
    if ((b & 15) === 2) doors.push(byte(p + 1));
    p += lengths[b & 15];
  }
  return { tiles, attributes, doors };
}
const DOOR_COLORS = [0x16, 0x12, 0x27, 0x12];
function renderRoom({ tiles, attributes, doors }, chr, colors) {
  const out = new Uint8ClampedArray(SCREEN_WIDTH * SCREEN_HEIGHT * 4);
  const put = (x, y, rgb) => {
    const o = (y * SCREEN_WIDTH + x) * 4;
    out[o] = rgb >> 16;
    out[o + 1] = (rgb >> 8) & 255;
    out[o + 2] = rgb & 255;
    out[o + 3] = 255;
  };
  for (let ty = 0; ty < 30; ty++)
    for (let tx = 0; tx < 32; tx++) {
      const tile = tiles[ty * 32 + tx], attribute = attributes[(ty >> 2) * 8 + (tx >> 2)];
      const sub = (attribute >> (2 * ((ty & 2 ? 2 : 0) + (tx & 2 ? 1 : 0)))) & 3, base = 0x1000 + tile * 16;
      for (let py = 0; py < 8; py++) {
        const lo = chr[base + py], hi = chr[base + py + 8];
        for (let px = 0; px < 8; px++) {
          const c = ((lo >> (7 - px)) & 1) | (((hi >> (7 - px)) & 1) << 1);
          put(tx * 8 + px, ty * 8 + py, NES_RGB[(c ? colors[sub * 4 + c] : colors[0]) & 0x3f]);
        }
      }
    }
  for (const info of doors) {
    const x = info & 0x10 ? 8 : 240, rgb = NES_RGB[DOOR_COLORS[info & 3]];
    for (let y = 80; y < 128; y++) for (let dx = 0; dx < 8; dx++) put(x + dx, y, rgb);
  }
  return out;
}

export function buildMap(engine, level) {
  const area = AREAS[level], rom = engine.rom, chr = patterns(rom, area.gfx), colors = palette(rom, area.bank);
  const world = rom.subarray(offset(0, 0xa53e), offset(0, 0xa53e) + 1024);
  const tiles = area.cells.map(([cx, cy]) => ({
    x: (cx - area.x0) * SCREEN_WIDTH,
    y: (cy - area.y0) * SCREEN_HEIGHT,
    width: SCREEN_WIDTH,
    height: SCREEN_HEIGHT,
    data: renderRoom(roomTiles(rom, area.bank, world[cy * 32 + cx]), chr, colors),
  }));
  return { ...MAP_SIZES[level], tiles, reached: tiles.length };
}

function bootToPlay(engine) {
  const ram = engine.ramView();
  engine.run(0, 300);
  for (let press = 0; press < 2; press++) {
    engine.run(START, 4);
    engine.run(0, 180);
  }
  for (let wait = 0; wait < 600 && !inPlay(ram); wait++) engine.run(0, 1);
}

export function buildSprites(engine) {
  engine.boot(0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = (ram[SAMUS_X] - ram[SCROLL_X]) & 255,
    sy = ram[SAMUS_Y] - 8;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    for (let i = 0x200; i < 0x300; i += 4) ram[i] = 0xf8;
    engine.commitMemory();
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    size = 48,
    sheet = new Uint8ClampedArray(size * size * 2 * 4),
    facesRight = !ram[FACING];
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size; x++) {
      const px = sx - size / 2 + x, py = sy - size / 2 + y;
      if (px < 0 || px >= 256 || py < 0 || py >= 224) continue;
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
  id: "metroid",
  title: "Metroid",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 131088,
    sha256: "e6e6b7014685adae447ebb3833242815747bc1e5df83ade79f693fb67cf565b6",
  },
  ramBytes: 0x2800,
  cartridgeMemory: true,
  levelCount: 1,
  grid: true,
  route: routeOrder,
  entry,
  pace: 120,
  system: "NES",
  assets: "",
  screen: { width: 256, height: 240 },
  closeUp: { width: 320, height: 240 },
  sprite: { width: 48, height: 48, originX: 16, originY: 8, icon: [12, 8, 24, 32] },
  controls: { a: "Jump", b: "Fire", select: "Missiles" },
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Metroid</h2><p>Metroid is © Nintendo. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The area maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Nintendo.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: AREAS.map((area, id) => ({ id, name: area.name, label: area.name, ...MAP_SIZES[id] })),
    levels: [{ id: 0, label: "Zebes", rooms: AREAS.map((_, id) => id) }],
  },
  buildMap,
  buildSprites,
  decode,
  ended: (ram) => dead(ram) || won(ram),
  levelComplete: () => false,
  transition: () => [],
  motion(ram) {
    const [level, x, y] = mapPoint(ram), valid = inPlay(ram) && !dead(ram) && (ram[DOOR] === 0 || ram[DOOR] === 5);
    return [valid ? level : 65535, x, y, ram[FACING] ? 1 : 0];
  },
  boot(engine) {
    bootToPlay(engine);
  },
};
