// SPDX-License-Identifier: AGPL-3.0-or-later
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { pathToFileURL } from "node:url";
import { createEngine } from "../src/emulator.js";
import tobu, { MAP_HEIGHTS, LEVEL_NAMES } from "../src/games/tobu-tobu-girl.js";
import { png } from "./png.mjs";
import { creditPNG } from "../src/media.js";

const CREDIT = {
  title: "Tobu Tobu Girl Deluxe",
  author: "Tangram Games",
  source: "https://github.com/SimonLarsen/tobutobugirl-dx/tree/4985d4d282aa43229f250ffa3b0d86998ebd55d3",
  code_license: "MIT",
  artwork:
    "Images by Tangram Games, CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/). Modified: captured from the game's screens, composed into level maps and a sprite sheet, and stretched to each level's height.",
};
const WIDTH = 144,
  BACKGROUND_ROWS = 256,
  SPRITE = 16,
  MARGIN = 4,
  PREVIEW_ROWS = [176, 104, 8],
  BUTTONS = [0, 0x80, 0x40, 0x10, 0x20, 1, 2, 0x81, 0x41, 0x11, 0x82, 0x42, 0x12];

const demo = pathToFileURL(process.cwd() + "/"),
  base = new URL("public/", demo),
  out = new URL(tobu.assets, base);
const game = { ...tobu, core: process.env.CORE || tobu.core };
const engine = await createEngine(base, game, {
  rom: new Uint8Array(await readFile(new URL(tobu.rom.file, base))),
  wasmBinary: await readFile(new URL(`engine/${game.core}.wasm`, base)),
});
const ram = engine.ramView();
const alive = () => ram[0xa5] === 4 && ram[0xa6] === 0;
const complete = () => ram[0xa5] === 4 && ram[0xa6] === 2;
const climb = () => ram[0x233] * ram[0x237] + ram[0x230];
const holdTimer = () => (ram[0xb0] = 32);
let seed = 12345;
const random = (n) => {
  seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
  return (seed >>> 8) % n;
};

function climbLevel(level) {
  engine.boot(level);
  const stack = [{ state: engine.capture(), path: [] }];
  for (let tries = 0; tries < 50000; tries++) {
    const top = stack[stack.length - 1];
    let best = null;
    for (let c = 0; c < 12; c++) {
      engine.restore(top.state);
      const plan = [0, 1].map(() => [BUTTONS[random(BUTTONS.length)], 4 + random(12)]);
      let survived = true;
      for (const [buttons, frames] of plan) {
        holdTimer();
        engine.step(buttons, frames);
        if (!alive()) {
          survived = false;
          break;
        }
      }
      if (complete()) return [...top.path, ...plan];
      if (!survived) continue;
      const score = climb() * 4 + ram[0x23a] / 8 - Math.abs(ram[0xc3] - 84);
      if (!best || score > best.score) best = { plan, score, state: engine.capture() };
    }
    if (best) stack.push({ state: best.state, path: [...top.path, ...best.plan] });
    else stack.length = Math.max(1, stack.length - 4);
  }
  throw new Error(`Could not climb ${LEVEL_NAMES[level]}`);
}

function replay(level, path, visit) {
  engine.boot(level);
  for (const [buttons, frames] of path) {
    holdTimer();
    for (let i = 0; i < frames; i++) {
      engine.run(buttons, 1, true);
      if (alive()) visit(engine.pixels(), 112 - ram[0x233]);
    }
  }
}

function mode(counts) {
  let best = 0, most = -1, total = 0;
  for (const [key, n] of counts) {
    total += n;
    if (n > most) [best, most] = [key, n];
  }
  return { key: best, share: total ? most / total : 0, total };
}
const pixelKey = (data, offset) => (data[offset] << 16) | (data[offset + 1] << 8) | data[offset + 2];
const rgba = (key, alpha = 255) => [key >> 16, (key >> 8) & 255, key & 255, alpha];

function background(level, path) {
  const votes = Array.from({ length: BACKGROUND_ROWS * WIDTH }, () => new Map());
  replay(level, path, (frame, scroll) => {
    for (let y = 0; y < frame.height; y++)
      for (let x = 0; x < WIDTH; x++) {
        const counts = votes[((scroll + y) % BACKGROUND_ROWS) * WIDTH + x], key = pixelKey(frame.data, (y * frame.width + x) * 4);
        counts.set(key, (counts.get(key) || 0) + 1);
      }
  });
  const keys = votes.map((counts) => mode(counts));
  if (keys.some((m) => !m.total)) throw new Error(`${LEVEL_NAMES[level]} background has rows the climb never showed`);
  return keys.map((m) => m.key);
}

function sprites(level, path, backdrop, patches) {
  replay(level, path, (frame, scroll) => {
    const left = ram[0xc2] - 24 - MARGIN, top = ram[0xc3] - 19 - MARGIN, facing = ram[0xc4] === 4 ? 1 : 0, size = SPRITE + 2 * MARGIN;
    if (left < 0 || left + size > WIDTH || top < 0 || top + size > frame.height) return;
    const patch = new Int32Array(size * size);
    for (let y = 0; y < size; y++)
      for (let x = 0; x < size; x++) {
        const key = pixelKey(frame.data, ((top + y) * frame.width + left + x) * 4);
        patch[y * size + x] = key === backdrop[((scroll + top + y) % BACKGROUND_ROWS) * WIDTH + left + x] ? -1 : key;
      }
    const id = `${facing}:${patch.join()}`, seen = patches.get(id);
    if (seen) seen.count++;
    else patches.set(id, { facing, patch, count: 1 });
  });
}

await mkdir(new URL("maps/", out), { recursive: true });
const maps = [],
  patches = new Map();
for (let level = 0; level < tobu.levelCount; level++) {
  const path = climbLevel(level), backdrop = background(level, path), height = MAP_HEIGHTS[level];
  sprites(level, path, backdrop, patches);
  const data = new Uint8Array(WIDTH * height * 4);
  for (let y = 0; y < height; y++) {
    const source = ((y + 0.5) * BACKGROUND_ROWS) / height - 0.5,
      upper = Math.max(0, Math.floor(source)),
      lower = Math.min(BACKGROUND_ROWS - 1, upper + 1),
      mix = Math.min(1, Math.max(0, source - upper));
    for (let x = 0; x < WIDTH; x++) {
      const a = rgba(backdrop[upper * WIDTH + x]), b = rgba(backdrop[lower * WIDTH + x]);
      data.set(a.map((v, i) => Math.round(v + (b[i] - v) * mix)), (y * WIDTH + x) * 4);
    }
  }
  const image = await creditPNG(new Blob([png(WIDTH, height, data)]), "Level map: the level background stretched to the level's height", CREDIT);
  await writeFile(new URL(`maps/${level}.png`, out), new Uint8Array(await image.arrayBuffer()));
  const preview = new Uint8Array(256 * 96 * 4);
  for (let y = 0; y < 96; y++)
    for (let x = 0; x < 256; x++) {
      const row = PREVIEW_ROWS[level] + Math.floor((y * 54) / 96), column = Math.floor((x * WIDTH) / 256);
      preview.set(rgba(backdrop[row * WIDTH + column]), (y * 256 + x) * 4);
    }
  const thumbnail = await creditPNG(new Blob([png(256, 96, preview)]), "Level selector thumbnail from the level background", CREDIT);
  await writeFile(new URL(`maps/${level}-preview.png`, out), new Uint8Array(await thumbnail.arrayBuffer()));
  maps.push({ id: level, name: LEVEL_NAMES[level], label: `Level ${level + 1}`, width: WIDTH, height, file: `maps/${level}.png` });
  console.log("Composed", level, LEVEL_NAMES[level], WIDTH, height, `${path.reduce((n, [, f]) => n + f, 0)} climb frames`);
}
const sheet = new Uint8Array(SPRITE * SPRITE * 2 * 4);
for (const facing of [0, 1]) {
  const best = [...patches.values()].filter((p) => p.facing === facing).sort((a, b) => b.count - a.count)[0];
  if (!best) throw new Error("The player sprite was never fully on screen");
  const size = SPRITE + 2 * MARGIN;
  for (let y = 0; y < size; y++)
    for (let x = 0; x < size; x++) {
      const key = best.patch[y * size + x];
      if (key < 0 || x < MARGIN || x >= MARGIN + SPRITE || y < MARGIN || y >= MARGIN + SPRITE) continue;
      sheet.set(rgba(key), ((facing * SPRITE + y - MARGIN) * SPRITE + x - MARGIN) * 4);
    }
  console.log("Player sprite", facing ? "left" : "right", `seen in ${best.count} frames`);
}
const sheetImage = await creditPNG(new Blob([png(SPRITE, SPRITE * 2, sheet)]), "Player sprite sheet captured from the game's screens", CREDIT);
await writeFile(new URL("sprites.png", out), new Uint8Array(await sheetImage.arrayBuffer()));
const levels = maps.map((map) => ({ id: map.id, label: map.label, rooms: [map.id] }));
await writeFile(new URL("maps.json", out), JSON.stringify({ maps, levels }));
