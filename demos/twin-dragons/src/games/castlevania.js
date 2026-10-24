// SPDX-License-Identifier: AGPL-3.0-or-later
import { castlevania_observation } from "../../rust/pkg/twin_dragons_browser.js";

const MODE = 0x18,
  SUBMODE = 0x19,
  STAGE = 0x28,
  X_LOW = 0x40,
  X_HIGH = 0x41,
  SECTION = 0x46,
  SIMON_Y = 0x3f,
  HEALTH = 0x45,
  INVINCIBLE = 0x5b,
  SCROLL = 0xfd,
  OAM = 0x200,
  MODE_PLAYING = 5,
  SUBMODE_CONTROL = 6,
  MODE_DOOR = 0x0a,
  FULL_HEALTH = 0x40,
  IMMUNE = 0x30,
  STEP = 2,
  STANDING_Y = 0xb0,
  STAGE_COUNT = 18,
  SCREEN_WIDTH = 256,
  IMAGE_HEIGHT = 224,
  HUD_ROWS = 40,
  MAP_HEIGHT = IMAGE_HEIGHT - HUD_ROWS,
  CROP_TOP = 8,
  SPRITE_SIZE = 32;

const MAP_CAPACITY = 4096,
  MAX_FRAMES = 8000,
  STILL_FRAMES = 240,
  BLOCKED_FRAMES = 24,
  NUDGE = 8,
  RIGHT = 0x80;

// Stage map widths in image pixels, measured by walking each stage once.
export const MAP_WIDTHS = [1536, 512, 768, 256, 256, 256, 256, 768, 1536, 1015, 1536, 408, 512, 256, 256, 256, 256, 256];

function stageName(id) {
  return `Stage ${id + 1}`;
}

export function levelName(id) {
  return stageName(id);
}

export function decode(memory) {
  return JSON.parse(castlevania_observation(memory));
}

export function entry() {
  return { x: 64, y: STANDING_Y - CROP_TOP - HUD_ROWS };
}

function position(ram) {
  return ram[X_LOW] | (ram[X_HIGH] << 8);
}

function scrollOf(x, low) {
  const base = x - SCREEN_WIDTH / 2,
    up = (low - base) & 255;
  return Math.max(0, base + (up <= 128 ? up : up - 256));
}

function hideSprites(ram) {
  for (let i = OAM; i < OAM + 256; i += 4) ram[i] = 0xf8;
}

function loadStage(engine, stage) {
  const ram = engine.ramView();
  ram[STAGE] = stage - 1;
  ram[MODE] = MODE_DOOR;
  for (let frame = 0; frame < 600; frame++) {
    engine.run(0, 1, false);
    if (ram[MODE] === MODE_PLAYING && ram[SUBMODE] === SUBMODE_CONTROL && ram[STAGE] === stage) return;
  }
  throw new Error(`Castlevania did not load stage ${stage}`);
}

function boot(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= STAGE_COUNT) throw new Error("Unknown Castlevania stage");
  engine.boot(0);
  if (level > 0) loadStage(engine, level + 1);
}

export function buildMap(engine, level) {
  if (!Number.isInteger(level) || level < 0 || level >= STAGE_COUNT) throw new Error("Unknown Castlevania stage");
  engine.boot(0);
  loadStage(engine, level + 1);
  const ram = engine.ramView(),
    ground = ram[SIMON_Y],
    section = ram[SECTION];
  let scroll = 0,
    scrollLow = ram[SCROLL],
    blocked = 0,
    farthest = 0,
    still = 0,
    data = new Uint8ClampedArray(MAP_CAPACITY * MAP_HEIGHT * 4);
  for (let frame = 0; frame < MAX_FRAMES; frame++) {
    if (ram[MODE] !== MODE_PLAYING || ram[SUBMODE] !== SUBMODE_CONTROL || ram[SECTION] !== section) break;
    hideSprites(ram);
    ram[HEALTH] = FULL_HEALTH;
    ram[INVINCIBLE] = IMMUNE;
    ram[SIMON_Y] = ground;
    const before = position(ram);
    engine.run(RIGHT, 1, true);
    if (position(ram) > before) blocked = 0;
    else if (++blocked > BLOCKED_FRAMES) {
      const nudged = position(ram) + NUDGE;
      ram[X_LOW] = nudged & 255;
      ram[X_HIGH] = (nudged >> 8) & 255;
      blocked = 0;
    }
    const low = ram[SCROLL];
    scroll = Math.max(0, scroll + (((low - scrollLow + 128) & 255) - 128));
    scrollLow = low;
    if (scroll > farthest) {
      farthest = scroll;
      still = 0;
    } else if (++still > STILL_FRAMES) break;
    if (farthest + SCREEN_WIDTH > MAP_CAPACITY) break;
    const pixels = engine.pixelView();
    for (let row = HUD_ROWS; row < IMAGE_HEIGHT; row++)
      data.set(
        pixels.subarray(row * SCREEN_WIDTH * 4, (row + 1) * SCREEN_WIDTH * 4),
        ((row - HUD_ROWS) * MAP_CAPACITY + scroll) * 4,
      );
  }
  const width = farthest + SCREEN_WIDTH,
    cropped = new Uint8ClampedArray(width * MAP_HEIGHT * 4);
  for (let y = 0; y < MAP_HEIGHT; y++)
    cropped.set(data.subarray(y * MAP_CAPACITY * 4, (y * MAP_CAPACITY + width) * 4), y * width * 4);
  return { width, height: MAP_HEIGHT, data: cropped, reached: width };
}

export function buildSprites(engine) {
  boot(engine, 0);
  const ram = engine.ramView(),
    start = engine.capture();
  engine.run(0, 2, true);
  const shown = engine.pixels().data,
    sx = position(ram) - scrollOf(position(ram), ram[SCROLL]),
    sy = ram[SIMON_Y] - CROP_TOP;
  engine.restore(start);
  for (let frame = 0; frame < 2; frame++) {
    hideSprites(ram);
    engine.run(0, 1, true);
  }
  const hidden = engine.pixels().data,
    size = SPRITE_SIZE,
    sheet = new Uint8ClampedArray(size * size * 2 * 4);
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size; x++) {
      const px = sx - size / 2 + x,
        py = sy - size / 2 + y;
      if (px < 0 || px >= SCREEN_WIDTH || py < 0 || py >= IMAGE_HEIGHT) continue;
      const s = (py * SCREEN_WIDTH + px) * 4;
      if (shown[s] === hidden[s] && shown[s + 1] === hidden[s + 1] && shown[s + 2] === hidden[s + 2]) continue;
      sheet.set(shown.subarray(s, s + 4), (y * size + x) * 4);
      sheet.set(shown.subarray(s, s + 4), ((size + y) * size + size - 1 - x) * 4);
    }
  engine.restore(start);
  return { width: size, height: size * 2, data: sheet };
}

export default {
  id: "castlevania",
  title: "Castlevania",
  core: "quicknes",
  rom: {
    upload: true,
    bytes: 131088,
    sha256: "5e859d77bc0e3a9f30a7c14d2637b9c2aae6ce19f6520a7c9b252a86974f7800",
  },
  ramBytes: 0x800,
  levelCount: STAGE_COUNT,
  pace: 300,
  maxActionFrames: 60,
  menuButtons: true,
  system: "NES",
  assets: "",
  screen: { width: SCREEN_WIDTH, height: IMAGE_HEIGHT },
  closeUp: { width: 320, height: 240 },
  sprite: { width: SPRITE_SIZE, height: SPRITE_SIZE, originX: 16, originY: 16, icon: [8, 2, 16, 30] },
  controls: { a: "Jump", b: "Whip" },
  maxHealth: FULL_HEALTH,
  credits: `<span class="eyebrow">CREDITS & LICENSING</span><h2>Castlevania</h2><p>Castlevania is © Konami. This site does not include the game. It runs from a ROM you supply, which stays in this browser’s storage and is never uploaded. The stage maps and the player sprite are drawn in your browser from that ROM. This demonstration is not endorsed by Konami.</p><p>QuickNES library sources: LGPL-2.1-or-later. The compiled libretro browser core, with Harmony’s <a href="licenses/quicknes-idle-loop.patch">idle-loop patch</a>, is distributed under GPL-2.0 terms with a GPL-2.0-or-later C++ shim. The separate Harmony browser interface and Rust search code: AGPL-3.0-or-later.</p><p><a href="licenses/CREDITS.md">Full credits</a> · <a href="licenses/quicknes-source.tar.gz">QuickNES source</a> · <a href="licenses/harmony-source.tar.gz">Harmony source</a> · <a href="licenses/rust-dependencies.tar.gz">Rust dependency sources and notices</a> · <a href="https://github.com/pH14/harmony">Repository and build instructions</a></p>`,
  catalog: {
    maps: Array.from({ length: STAGE_COUNT }, (_, id) => ({ id, name: stageName(id), label: stageName(id), width: MAP_WIDTHS[id], height: MAP_HEIGHT })),
    levels: Array.from({ length: STAGE_COUNT }, (_, id) => ({ id, label: stageName(id), rooms: [id] })),
  },
  entry,
  buildMap,
  buildSprites,
  decode,
  campaign: true,
  boot,
};
