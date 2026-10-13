// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import metroid, { buildMap, buildSprites, MAP_SIZES } from "../src/games/metroid.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { RoundSearch, ROUND_PATHS } from "../src/rollout.js";
import init, { Explorer } from "../rust/pkg/twin_dragons_browser.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.METROID_ROM || `${homedir()}/workspace/roms/Metroid (USA).nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Metroid: no ROM at ${path}. Set METROID_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, metroid, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });
globalThis.harmonyEngine = engine;
await init({ module_or_path: await readFile(new URL("../rust/pkg/twin_dragons_browser_bg.wasm", import.meta.url)) });

const root = engine.boot(0);
const start = engine.observation();
assert.deepEqual([start.level, start.loading, start.health, start.cleared], [0, false, 30, 0], "A new game starts in Brinstar with 30 energy");
assert.deepEqual([start.x, start.y], [2 * 256 + 128, 13 * 240 + 176], "Samus starts in the Brinstar start room");
console.log("Metroid boots into Brinstar with cartridge RAM visible.");

for (let level = 0; level < MAP_SIZES.length; level++) {
  const map = buildMap(engine, level);
  assert.ok(map.tiles.length > 20, `${metroid.catalog.maps[level].name} has its rooms`);
  for (const tile of map.tiles) {
    assert.ok(tile.x + tile.width <= map.width && tile.y + tile.height <= map.height);
    assert.equal(tile.data.length, 256 * 240 * 4);
  }
}
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Samus");
console.log("Area maps and the sprite sheet build from the ROM.");

engine.restore(root);
const search = new Explorer(metroid.id, 3),
  rounds = new RoundSearch(search, [engine]);
let farthest = null;
for (let tries = 0; tries < 3000; tries += ROUND_PATHS) {
  const { batch } = rounds.round();
  for (const point of batch.points)
    if (point.retained !== null && (!farthest || Math.abs(point.observation.x - start.x) > Math.abs(farthest.observation.x - start.x))) farthest = point;
}
assert.ok(farthest, "The search retains states in Brinstar");
const state = JSON.parse(search.state(farthest.retained));
engine.restore(root);
for (const action of state.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), search.digest(farthest.retained), "The archived state replays exactly from its controller history");
const decoded = engine.observation();
for (const key of Object.keys(decoded)) assert.equal(decoded[key], state.observation[key], `Browser and Rust decoders agree on ${key}`);
console.log(`Brinstar: farthest state ${Math.abs(farthest.observation.x - start.x)} pixels from the start; exact replay passed.`);
search.free();

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=metroid&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
assert.match(await page.locator("#rom-message").innerText(), /Choose your Metroid ROM/);
const chooser = page.waitForEvent("filechooser");
await page.locator("#game-choice button", { hasText: "Metroid" }).click();
await (await chooser).setFiles(path);
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "metroid" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
const strip = page.locator('.level-overview[data-map="0"]'), view = page.locator('.area-map[data-map="0"]');
await page.waitForFunction(() => document.querySelector('.area-map[data-map="0"]')?.dataset.centerX);
const stripBox = await strip.boundingBox(), viewBox = await view.boundingBox();
assert.ok(stripBox.x >= viewBox.x + viewBox.width, "The area map sits beside the game view");
const center = async () => [Number(await view.getAttribute("data-center-x")), Number(await view.getAttribute("data-center-y"))];
await page.mouse.click(stripBox.x + stripBox.width * 0.2, stripBox.y + stripBox.height * 0.8);
const low = await center();
await page.mouse.click(stripBox.x + stripBox.width * 0.8, stripBox.y + stripBox.height * 0.2);
const high = await center();
assert.ok(high[0] > low[0] && high[1] < low[1], `Clicking the area map moves the view across and up (${low} to ${high})`);
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts a Metroid search, and the area map moves the view in both directions.");
await browser.close();
