// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { availableParallelism } from "node:os";
import { Worker, isMainThread, parentPort, workerData } from "node:worker_threads";
import { createEngine } from "../src/emulator.js";
import famidash, { LEVEL_NAMES } from "../src/games/famidash.js";
import { famidash_map_top } from "../rust/pkg/twin_dragons_browser.js";
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
const GAME_STATE = 0x498,
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
  SCREEN_ROWS = 240,
  CROP = 8,
  SCROLL_LAG = 1,
  MAP_HEIGHT = 720,
  CHUNK_PATTERNS = [[[0, 8]], [[1, 8]], [[1, 1], [0, 7]], [[1, 4], [0, 4]], [[0, 4], [1, 4]], [[0, 6], [1, 2]]],
  BEAMS = [12, 48, 128],
  PREFERRED_COLUMN = 176,
  SPRITE = famidash.sprite.width,
  MODES = 8,
  SPRITE_FRAME = 30,
  INVINCIBLE = 0x4a1,
  PPU_MASK = 0x0b,
  SHOW_BACKGROUND = 0x0a;

const demo = pathToFileURL(process.cwd() + "/"),
  base = new URL("public/", demo),
  out = new URL(famidash.assets, base);
const engine = await createEngine(base, famidash, {
  rom: new Uint8Array(await readFile(new URL(famidash.rom.file, base))),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const ram = engine.ramView();
const MAP_TOP = famidash_map_top();
const scrollX = () => ram[SCROLL_X] | (ram[SCROLL_X + 1] << 8) | (ram[SCROLL_X + 2] << 16) | (ram[SCROLL_X + 3] << 24);
const cameraY = () => ram[SCROLL_Y + 1] * SCREEN_ROWS + ram[SCROLL_Y];
const dead = () => ((ram[CUBE_DATA] | ram[CUBE_DATA + 1]) & 1) !== 0;
const velocity = () => ((ram[VELOCITY_Y] | (ram[VELOCITY_Y + 1] << 8)) << 16) >> 16;

function beamSearch(level, width) {
  engine.boot(level);
  let beam = [{ state: engine.capture(), parent: null, pattern: -1, y: 0 }];
  for (;;) {
    const next = new Map();
    for (const node of beam)
      for (let p = 0; p < CHUNK_PATTERNS.length; p++) {
        engine.restore(node.state);
        let alive = true;
        for (const [buttons, frames] of CHUNK_PATTERNS[p]) {
          for (let i = 0; i < frames && alive; i++) {
            engine.step(buttons, 1);
            if (ram[GAME_STATE] === COMPLETE) {
              const path = [p];
              for (let n = node; n.parent; n = n.parent) path.unshift(n.pattern);
              return path;
            }
            alive = ram[GAME_STATE] === PLAYING && !dead();
          }
          if (!alive) break;
        }
        if (!alive) continue;
        const y = cameraY() + ram[PLAYER_Y];
        const second = ram[DUAL] ? [ram[PLAYER_Y + 2] >> 2, ram[VELOCITY_Y + 3]] : [];
        const key = [y >> 2, velocity() >> 7, ram[GAMEMODE], ram[GRAVITY], ram[MINI], ram[DUAL], ...second].join();
        if (!next.has(key)) next.set(key, { state: engine.capture(), parent: node, pattern: p, y });
      }
    if (!next.size) return null;
    beam = [...next.values()].sort((a, b) => a.y - b.y);
    if (beam.length > width) beam = Array.from({ length: width }, (_, i) => beam[Math.floor((i * (beam.length - 1)) / (width - 1))]);
  }
}

function clearLevel(level) {
  for (const width of BEAMS) {
    const path = beamSearch(level, width);
    if (path) return path;
  }
  throw new Error(`Could not clear ${LEVEL_NAMES[level]}`);
}

function replay(level, path, visit) {
  engine.boot(level);
  const history = [];
  for (const p of path)
    for (const [buttons, frames] of CHUNK_PATTERNS[p])
      for (let i = 0; i < frames; i++) {
        engine.run(buttons, 1, true);
        history.push({ scroll: scrollX(), camera: cameraY(), x: ram[PLAYER_X], y: ram[PLAYER_Y] });
        if (history.length > SCROLL_LAG + 1) history.shift();
        if (ram[GAME_STATE] !== PLAYING || dead()) return;
        const view = history[0];
        if (view.scroll >= 0 && view.camera > 0)
          visit(engine.pixels(), view.scroll, view.camera - MAP_TOP + CROP, view.x, view.y - CROP);
      }
}

const rgba = (data, offset) => (data[offset] << 24) | (data[offset + 1] << 16) | (data[offset + 2] << 8) | data[offset + 3];

function compose(level, path) {
  let width = 0;
  replay(level, path, (frame, left) => (width = Math.max(width, left + frame.width)));
  const pixels = new Uint32Array(width * MAP_HEIGHT),
    quality = new Uint16Array(width * MAP_HEIGHT).fill(0xffff);
  let top = MAP_HEIGHT;
  replay(level, path, (frame, left, row, playerX, playerY) => {
    top = Math.min(top, Math.max(0, row));
    for (let y = 0; y < frame.height; y++) {
      const my = row + y;
      if (my < 0 || my >= MAP_HEIGHT) continue;
      for (let x = CROP; x < frame.width; x++) {
        if (x >= playerX - 8 && x < playerX + 24 && y >= playerY - 8 && y < playerY + 24) continue;
        const mx = left + x;
        if (mx < 0 || mx >= width) continue;
        const index = my * width + mx, q = Math.abs(x - PREFERRED_COLUMN);
        if (q > quality[index]) continue;
        quality[index] = q;
        pixels[index] = rgba(frame.data, (y * frame.width + x) * 4);
      }
    }
  });
  for (let x = 0; x < width; x++) {
    let last = 0;
    for (let y = 0; y < MAP_HEIGHT; y++) {
      const index = y * width + x;
      if (quality[index] !== 0xffff) last = pixels[index];
      else if (last) pixels[index] = last;
    }
    last = 0;
    for (let y = MAP_HEIGHT - 1; y >= 0; y--) {
      const index = y * width + x;
      if (quality[index] !== 0xffff) last = pixels[index];
      else if (last && !pixels[index]) pixels[index] = last;
    }
  }
  for (let y = 0; y < MAP_HEIGHT; y++) pixels.fill(pixels[y * width + CROP], y * width, y * width + CROP);
  return { width, pixels, top };
}

function spriteSheet() {
  const sheet = new Uint8Array(SPRITE * MODES * SPRITE * 2 * 4);
  engine.boot(0);
  engine.run(0, SPRITE_FRAME, true);
  const start = engine.capture();
  for (let mode = 0; mode < MODES; mode++) {
    engine.restore(start);
    ram[GAMEMODE] = mode;
    for (let i = 0; i < 4; i++) {
      ram[INVINCIBLE] = 8;
      ram[PPU_MASK] &= ~SHOW_BACKGROUND;
      engine.run(0, 1, true);
    }
    const frame = engine.pixels(),
      left = ram[PLAYER_X] - famidash.sprite.originX,
      top = ram[PLAYER_Y] - CROP - famidash.sprite.originY,
      counts = new Map(),
      patch = [];
    for (let y = 0; y < SPRITE; y++)
      for (let x = 0; x < SPRITE; x++) {
        const key = rgba(frame.data, ((top + y) * frame.width + left + x) * 4);
        patch.push(key);
        counts.set(key, (counts.get(key) || 0) + 1);
      }
    const backdrop = [...counts].sort((a, b) => b[1] - a[1])[0][0];
    if (patch.every((key) => key === backdrop)) throw new Error(`The player sprite for mode ${mode} is empty`);
    for (let y = 0; y < SPRITE; y++)
      for (let x = 0; x < SPRITE; x++) {
        const key = patch[y * SPRITE + x];
        if (key === backdrop) continue;
        for (const [row, sy] of [[0, y], [1, SPRITE - 1 - y]])
          sheet.set([key >>> 24, (key >>> 16) & 255, (key >>> 8) & 255, 255], ((row * SPRITE + sy) * SPRITE * MODES + mode * SPRITE + x) * 4);
      }
  }
  return sheet;
}

function bytes(pixels) {
  const data = new Uint8Array(pixels.length * 4);
  for (let i = 0; i < pixels.length; i++) {
    const p = pixels[i];
    data[i * 4] = p >>> 24;
    data[i * 4 + 1] = (p >>> 16) & 255;
    data[i * 4 + 2] = (p >>> 8) & 255;
    data[i * 4 + 3] = p & 255;
  }
  return data;
}

async function buildLevel(level) {
  const path = clearLevel(level),
    map = compose(level, path);
  const image = await creditPNG(
    new Blob([png(map.width, MAP_HEIGHT, bytes(map.pixels))]),
    "Level map: the screens of one cleared run composed into the level",
    CREDIT,
  );
  await writeFile(new URL(`maps/${level}.png`, out), new Uint8Array(await image.arrayBuffer()));
  const preview = new Uint32Array(256 * 96),
    previewTop = MAP_HEIGHT - 384;
  for (let y = 0; y < 96; y++)
    for (let x = 0; x < 256; x++) preview[y * 256 + x] = map.pixels[(previewTop + y * 4) * map.width + x * 4];
  const thumbnail = await creditPNG(new Blob([png(256, 96, bytes(preview))]), "Level selector thumbnail from the level map", CREDIT);
  await writeFile(new URL(`maps/${level}-preview.png`, out), new Uint8Array(await thumbnail.arrayBuffer()));
  console.log("Composed", level, LEVEL_NAMES[level], map.width, `highest camera row ${map.top}`, `${path.length * 8} frames`);
  return { id: level, name: LEVEL_NAMES[level], label: LEVEL_NAMES[level], width: map.width, height: MAP_HEIGHT, file: `maps/${level}.png` };
}

if (isMainThread) {
  await mkdir(new URL("maps/", out), { recursive: true });
  const maps = [],
    threads = Math.min(availableParallelism(), famidash.levelCount);
  await Promise.all(
    Array.from(
      { length: threads },
      (_, thread) =>
        new Promise((resolve, reject) => {
          const levels = Array.from({ length: famidash.levelCount }, (_, level) => level).filter((level) => level % threads === thread);
          new Worker(new URL(import.meta.url), { workerData: levels })
            .on("message", (map) => (maps[map.id] = map))
            .on("error", reject)
            .on("exit", (code) => (code ? reject(new Error(`A map thread exited with code ${code}`)) : resolve()));
        }),
    ),
  );
  const sheet = spriteSheet();
  const sheetImage = await creditPNG(new Blob([png(SPRITE * MODES, SPRITE * 2, sheet)]), "Player sprite sheet captured from the game's screens", CREDIT);
  await writeFile(new URL("sprites.png", out), new Uint8Array(await sheetImage.arrayBuffer()));
  const levels = maps.map((map) => ({ id: map.id, label: map.label, rooms: [map.id] }));
  await writeFile(new URL("maps.json", out), JSON.stringify({ maps, levels }));
} else for (const level of workerData) parentPort.postMessage(await buildLevel(level));
