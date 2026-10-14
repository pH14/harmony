// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine } from "../src/emulator.js";
import famidash, { LEVEL_NAMES } from "../src/games/famidash.js";
import { famidash_level_select_tape, famidash_map_top } from "../rust/pkg/twin_dragons_browser.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";

const CREDIT = {
  title: "Famidash",
  author: "TFDSoft",
  source: "https://github.com/tfdsoft/famidash/tree/0df680d0e02f0ccb438f1224d162a0ab073b7204",
  code_license: "MIT",
  artwork:
    "Captured from the screens of Famidash 1.2.8 by TFDSoft, MIT license. Modified: composed into level maps and a sprite sheet. Famidash is an unofficial fan demake of Geometry Dash by RobTop Games.",
};
const LEVEL = 0x47b,
  GAME_STATE = 0x498,
  COMPLETE = 3,
  PLAYING = 2,
  GAMEMODE = 0x7a,
  CUBE_DATA = 0x7b,
  PLAYER_X = 0x43e,
  PLAYER_Y = 0x442,
  VELOCITY_Y = 0x449,
  GRAVITY = 0x44d,
  MINI = 0x44f,
  DUAL = 0x96,
  SCROLL_X = 0x4a2,
  SCROLL_Y = 0x4a6,
  PARALLAX_SCROLL_X = 0x4a0,
  PALETTE = 0x1a0,
  PALETTE_UPDATE = 0x03,
  PARALLAX_COLORS = [0x1a1, 0x1a9],
  A = 1,
  SCREEN_ROWS = 240,
  CROP = 8,
  MARGIN = 16,
  WORLD_ROWS = 1440,
  RENDER_EVERY = 16,
  PREFERRED_COLUMN = 176,
  UNSEEN = 255,
  CHUNK_PATTERNS = [[[0, 8]], [[1, 8]], [[1, 1], [0, 7]], [[1, 4], [0, 4]], [[0, 4], [1, 4]], [[0, 6], [1, 2]]],
  REPAIR_PATTERNS = [2, 1, 3, 5, 4],
  REPAIR_AHEAD = 2,
  REPAIR_SINGLE = 8,
  REPAIR_PAIR = 4,
  BEAM_WIDTH = 12,
  BEAM_BACKS = [8, 24, 64, Infinity],
  SPRITE = famidash.sprite.width,
  MODES = 8,
  SPRITE_FRAME = 30,
  INVINCIBLE = 0x4a1,
  PPU_MASK = 0x0b,
  SHOW_BACKGROUND = 0x0a,
  ROUTES = new URL("famidash-routes.json", import.meta.url);

const demo = pathToFileURL(process.cwd() + "/"),
  base = new URL("public/", demo),
  out = new URL(famidash.assets, base);
const engine = await createEngine(base, famidash, {
  rom: new Uint8Array(await readFile(new URL(famidash.rom.file, base))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const ram = engine.ramView();
const scrollX = () => ram[SCROLL_X] | (ram[SCROLL_X + 1] << 8) | (ram[SCROLL_X + 2] << 16) | (ram[SCROLL_X + 3] << 24);
const cameraY = () => ram[SCROLL_Y + 1] * SCREEN_ROWS + ram[SCROLL_Y];
const dead = () => ((ram[CUBE_DATA] | ram[CUBE_DATA + 1]) & 1) !== 0;
const velocity = () => ((ram[VELOCITY_Y] | (ram[VELOCITY_Y + 1] << 8)) << 16) >> 16;

engine.reset();
for (const { buttons, frames } of JSON.parse(famidash_level_select_tape())) engine.step(buttons, frames);
const levelSelect = engine.capture();

function boot(level) {
  engine.restore(levelSelect);
  ram[LEVEL] = level;
  engine.step(A, 1);
  engine.step(0, 90);
  if (ram[GAME_STATE] !== PLAYING || ram[LEVEL] !== level) throw new Error(`Setup did not reach ${LEVEL_NAMES[level]}`);
}

function frame(buttons, render) {
  ram[PARALLAX_SCROLL_X] = 0;
  for (const color of PARALLAX_COLORS) ram[color] = ram[PALETTE];
  ram[PALETTE_UPDATE] = 1;
  engine.run(buttons, 1, render);
  if (ram[GAME_STATE] === COMPLETE) return 1;
  return ram[GAME_STATE] !== PLAYING || dead() ? -1 : 0;
}

function chunk(pattern, onFrame) {
  for (const [buttons, frames] of CHUNK_PATTERNS[pattern])
    for (let i = 0; i < frames; i++) {
      const result = onFrame ? onFrame(buttons) : frame(buttons, false);
      if (result) return result;
    }
  return 0;
}

function beam(start, steps) {
  let nodes = [{ state: start, parent: null, pattern: -1, y: 0 }];
  for (let step = 0; step < steps; step++) {
    const next = new Map();
    for (const node of nodes)
      for (let pattern = 0; pattern < CHUNK_PATTERNS.length; pattern++) {
        engine.restore(node.state);
        const result = chunk(pattern);
        if (result < 0) continue;
        const path = [pattern];
        for (let n = node; n.parent; n = n.parent) path.unshift(n.pattern);
        if (result > 0) return path;
        const y = cameraY() + ram[PLAYER_Y];
        const second = ram[DUAL] ? [ram[PLAYER_Y + 2] >> 2, ram[VELOCITY_Y + 3]] : [];
        const key = [y >> 2, velocity() >> 7, ram[GAMEMODE], ram[GRAVITY], ram[MINI], ram[DUAL], ...second].join();
        if (!next.has(key)) next.set(key, { state: engine.capture(), parent: node, pattern, y });
      }
    if (!next.size) return null;
    nodes = [...next.values()].sort((a, b) => a.y - b.y);
    if (nodes.length > BEAM_WIDTH)
      nodes = Array.from({ length: BEAM_WIDTH }, (_, i) => nodes[Math.floor((i * (nodes.length - 1)) / (BEAM_WIDTH - 1))]);
  }
  const path = [];
  for (let n = nodes[0]; n.parent; n = n.parent) path.unshift(n.pattern);
  return path;
}

function search(level) {
  boot(level);
  const states = [engine.capture()],
    path = [];
  const reach = (from, plan, target) => {
    engine.restore(states[from]);
    let depth = from;
    for (let i = 0; depth < target; i++, depth++) {
      const result = chunk(plan[i] ?? 0);
      if (result) return result > 0 ? Infinity : depth;
    }
    return depth;
  };
  const commit = (from, plan) => {
    path.length = from;
    states.length = from + 1;
    engine.restore(states[from]);
    for (const pattern of plan) {
      const result = chunk(pattern);
      path.push(pattern);
      if (result > 0) return true;
      if (result < 0) throw new Error(`The repaired route for ${LEVEL_NAMES[level]} diverged`);
      states.push(engine.capture());
    }
    return false;
  };
  for (;;) {
    engine.restore(states[path.length]);
    const result = chunk(0);
    path.push(0);
    if (result > 0) return path;
    if (result === 0) {
      states.push(engine.capture());
      continue;
    }
    path.pop();
    const depth = path.length,
      target = depth + REPAIR_AHEAD;
    let plan = null,
      from = 0;
    for (let back = 0; back < REPAIR_SINGLE && !plan && back <= depth; back++)
      for (const pattern of REPAIR_PATTERNS) {
        const candidate = [pattern, ...Array(back).fill(0)];
        if (reach(depth - back, candidate, target) >= target) {
          [plan, from] = [candidate, depth - back];
          break;
        }
      }
    for (let back = 1; back < REPAIR_PAIR && !plan && back <= depth; back++)
      pairs: for (let gap = 1; gap <= back; gap++)
        for (const first of REPAIR_PATTERNS)
          for (const second of REPAIR_PATTERNS) {
            const candidate = Array(back + 1).fill(0);
            [candidate[0], candidate[gap]] = [first, second];
            if (reach(depth - back, candidate, target) >= target) {
              [plan, from] = [candidate, depth - back];
              break pairs;
            }
          }
    for (const back of BEAM_BACKS) {
      if (plan) break;
      from = Math.max(0, depth - back);
      plan = beam(states[from], depth - from + REPAIR_AHEAD + 2);
      if (from === 0) break;
    }
    if (!plan) throw new Error(`Could not clear ${LEVEL_NAMES[level]}`);
    if (commit(from, plan)) return path;
  }
}

const COLUMN_QUALITY = Uint8Array.from({ length: 256 }, (_, x) => Math.abs(x - PREFERRED_COLUMN));
let buffers = { pixels: new Uint32Array(0) };

function compose(level, route) {
  boot(level);
  const width = scrollX() + route.length * 8 * 8 + 256;
  if (buffers.pixels.length < width * WORLD_ROWS)
    buffers = { pixels: new Uint32Array(width * WORLD_ROWS), quality: new Uint8Array(width * WORLD_ROWS) };
  const { pixels, quality } = buffers;
  quality.fill(UNSEEN, 0, width * WORLD_ROWS);
  let frames = 0,
    right = 0,
    lowest = Infinity,
    highest = -Infinity,
    complete = false;
  const onFrame = (buttons) => {
    const left = scrollX(),
      camera = cameraY(),
      playerX = ram[PLAYER_X],
      playerY = ram[PLAYER_Y] - CROP,
      render = frames++ % RENDER_EVERY === 0;
    const result = frame(buttons, render);
    if (result) return result;
    if (!render || camera <= 0) return 0;
    const top = camera + CROP;
    if (top + 224 > WORLD_ROWS) throw new Error(`${LEVEL_NAMES[level]} scrolls below the map buffer`);
    lowest = Math.min(lowest, top);
    highest = Math.max(highest, top + 224);
    right = Math.max(right, left + 256);
    const view = engine.pixelView(),
      screen = new Uint32Array(view.buffer, view.byteOffset, 256 * 224);
    for (let y = 0; y < 224; y++) {
      const row = (top + y) * width + left,
        player = y >= playerY - 8 && y < playerY + 24;
      for (let x = CROP; x < 256; x++) {
        const index = row + x,
          q = COLUMN_QUALITY[x];
        if (q > quality[index] || (player && x >= playerX - 8 && x < playerX + 24)) continue;
        quality[index] = q;
        pixels[index] = screen[y * 256 + x];
      }
    }
    return 0;
  };
  for (const pattern of route)
    if (chunk(pattern, onFrame)) {
      complete = ram[GAME_STATE] === COMPLETE;
      break;
    }
  if (!complete) throw new Error(`The recorded route for ${LEVEL_NAMES[level]} no longer clears it; run with --search`);
  const top = Math.max(0, lowest - MARGIN),
    height = highest + MARGIN - top,
    mapWidth = right,
    map = new Uint32Array(mapWidth * height);
  const unseen = new Uint8Array(mapWidth),
    counts = new Map();
  for (let y = 0; y < height; y++)
    for (let x = 0; x < mapWidth; x++) {
      const index = (top + y) * width + Math.max(CROP, x);
      if (quality[index] === UNSEEN) unseen[x] = 1;
      else map[y * mapWidth + x] = pixels[index];
    }
  for (let x = 0; x < mapWidth; x++) {
    if (!unseen[x]) continue;
    const column = Math.max(CROP, x);
    counts.clear();
    for (let y = 0; y < height; y++) {
      const index = (top + y) * width + column;
      if (quality[index] !== UNSEEN) counts.set(pixels[index], (counts.get(pixels[index]) || 0) + 1);
    }
    let backdrop = 0,
      most = 0;
    for (const [color, count] of counts) if (count > most) [backdrop, most] = [color, count];
    for (let y = 0; y < height; y++) if (quality[(top + y) * width + column] === UNSEEN) map[y * mapWidth + x] = backdrop;
  }
  return { width: mapWidth, height, top, pixels: map, frames };
}

function spriteSheet() {
  const sheet = new Uint32Array(SPRITE * MODES * SPRITE * 2);
  boot(0);
  engine.step(0, SPRITE_FRAME);
  const start = engine.capture();
  for (let mode = 0; mode < MODES; mode++) {
    engine.restore(start);
    ram[GAMEMODE] = mode;
    for (let i = 0; i < 4; i++) {
      ram[INVINCIBLE] = 8;
      ram[PPU_MASK] &= ~SHOW_BACKGROUND;
      engine.run(0, 1, true);
    }
    const view = engine.pixelView(),
      screen = new Uint32Array(view.buffer, view.byteOffset, 256 * 224),
      left = ram[PLAYER_X] - famidash.sprite.originX,
      top = ram[PLAYER_Y] - CROP - famidash.sprite.originY,
      counts = new Map(),
      patch = [];
    for (let y = 0; y < SPRITE; y++)
      for (let x = 0; x < SPRITE; x++) {
        const color = screen[(top + y) * 256 + left + x];
        patch.push(color);
        counts.set(color, (counts.get(color) || 0) + 1);
      }
    const background = [...counts].sort((a, b) => b[1] - a[1])[0][0];
    if (patch.every((color) => color === background)) throw new Error(`The player sprite for mode ${mode} is empty`);
    for (let y = 0; y < SPRITE; y++)
      for (let x = 0; x < SPRITE; x++) {
        const color = patch[y * SPRITE + x];
        if (color === background) continue;
        for (const [row, sy] of [[0, y], [1, SPRITE - 1 - y]]) sheet[(row * SPRITE + sy) * SPRITE * MODES + mode * SPRITE + x] = color | 0xff000000;
      }
  }
  return sheet;
}

const image = (width, height, pixels, frame) =>
  creditPNG(new Blob([png(width, height, new Uint8Array(pixels.buffer, pixels.byteOffset, pixels.byteLength))]), frame, CREDIT).then(
    async (blob) => new Uint8Array(await blob.arrayBuffer()),
  );

async function buildLevel(level, route) {
  const map = compose(level, route),
    expected = famidash_map_top(level);
  await writeFile(new URL(`maps/${level}.png`, out), await image(map.width, map.height, map.pixels, "Level map: the screens of one cleared run composed into the level"));
  const preview = new Uint32Array(256 * 96),
    scale = Math.min(4, map.height / 96),
    previewTop = map.height - 96 * scale;
  for (let y = 0; y < 96; y++)
    for (let x = 0; x < 256; x++) preview[y * 256 + x] = map.pixels[Math.floor(previewTop + y * scale) * map.width + Math.floor(x * scale)];
  await writeFile(new URL(`maps/${level}-preview.png`, out), await image(256, 96, preview, "Level selector thumbnail from the level map"));
  console.log("Composed", level, LEVEL_NAMES[level], `${map.width}x${map.height}`, `top ${map.top}`, `${map.frames} frames`);
  return { id: level, name: LEVEL_NAMES[level], label: LEVEL_NAMES[level], width: map.width, height: map.height, file: `maps/${level}.png`, top: map.top, expected };
}

const levels = Array.from({ length: famidash.levelCount }, (_, level) => level);
if (process.argv.includes("--search")) {
  const routes = await readFile(ROUTES, "utf8").then(JSON.parse, () => ({ routes: [] }));
  const only = process.argv.slice(process.argv.indexOf("--search") + 1).filter((arg) => /^\d+$/.test(arg)).map(Number);
  for (const level of only.length ? only : levels) {
    const started = performance.now();
    routes.routes[level] = search(level).join("");
    await writeFile(ROUTES, JSON.stringify(routes, null, 1) + "\n");
    console.log("Cleared", level, LEVEL_NAMES[level], `${routes.routes[level].length} chunks`, `${((performance.now() - started) / 1000).toFixed(1)} s`);
  }
}
const { routes } = JSON.parse(await readFile(ROUTES, "utf8"));
await mkdir(new URL("maps/", out), { recursive: true });
const maps = [];
for (const level of levels) maps.push(await buildLevel(level, [...routes[level]].map(Number)));
const moved = maps.filter((map) => map.top !== map.expected);
if (moved.length)
  throw new Error(`The map tops changed for ${moved.map((map) => map.name).join(", ")}. Set MAP_TOPS in rust/src/famidash.rs to [${maps.map((map) => map.top).join(", ")}]`);
await writeFile(new URL("sprites.png", out), await image(SPRITE * MODES, SPRITE * 2, spriteSheet(), "Player sprite sheet captured from the game's screens"));
await writeFile(
  new URL("maps.json", out),
  JSON.stringify({
    maps: maps.map(({ id, name, label, width, height, file }) => ({ id, name, label, width, height, file })),
    levels: maps.map((map) => ({ id: map.id, label: map.label, rooms: [map.id] })),
  }),
);
