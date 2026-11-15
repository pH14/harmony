// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import castlevania, { buildMap, buildSprites, MAP_WIDTHS } from "../src/games/castlevania.js";
import { createEngine } from "../src/emulator.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.CASTLEVANIA_ROM || `${homedir()}/workspace/harmony/.arcade-local/roms/castlevania.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Castlevania: no ROM at ${path}. Set CASTLEVANIA_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, castlevania, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

engine.boot(0);
const start = castlevania.decode(engine.ramView());
assert.equal(start.stage, 0, "A new game starts in the courtyard before stage 1");
console.log("Castlevania boots into the courtyard.");

for (const level of [0, 1, 9]) {
  const map = buildMap(engine, level);
  assert.equal(castlevania.decode(engine.ramView()).stage, level + 1, "The walk leaves the engine in the stage it mapped");
  assert.equal(map.width, MAP_WIDTHS[level], `${castlevania.catalog.maps[level].name} is ${MAP_WIDTHS[level]} pixels wide`);
  assert.equal(map.data.length, map.width * map.height * 4);
}
console.log("Stage maps build from the ROM at their measured widths.");

for (let level = 0; level < castlevania.levelCount; level++) buildMap(engine, level);
console.log("Every stage enters at the camera its map starts from.");

const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Simon");
console.log("The sprite sheet builds from the ROM.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
const errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=castlevania&pace=0&workers=1`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 60000 });
await page.mouse.move(2, 890);
await page.waitForFunction(() => Number(document.querySelector("#hero-timelines")?.textContent.replace(/\D/g, "")) > 200, null, { timeout: 90000 });
const canvas = page.locator('canvas[data-map="0"]').first();
const box = await canvas.boundingBox();
let selected = false;
for (let row = 0.2; row < 0.9 && !selected; row += 0.1)
  for (let col = 0.05; col < 0.95 && !selected; col += 0.05) {
    await page.mouse.click(box.x + box.width * col, box.y + box.height * row);
    selected = (await page.locator("#inspector").getAttribute("data-empty")) === "false";
  }
assert.ok(selected, "Clicking a visible trail cell selects its history");
assert.match(await page.locator("#cell-title").getAttribute("title"), /^Cell \d+, \d+$/);
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Clicking a visible trail cell selects its history.");

const stage6 = await browser.newPage({ viewport: { width: 1280, height: 900 } });
stage6.on("pageerror", (error) => errors.push(error.message));
await stage6.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await stage6.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=castlevania&pace=0&workers=1`);
await stage6.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const stage6Chooser = stage6.waitForEvent("filechooser");
await stage6.locator("#rom-drop").click();
await (await stage6Chooser).setFiles(path);
await stage6.locator("#level-grid:not([hidden]) button", { hasText: /^Stage 6$/ }).click({ timeout: 60000 });
await stage6.waitForFunction(() => Number(document.querySelector('canvas[data-map="5"]')?.dataset.swarmCount) > 0, null, { timeout: 90000 });
console.log("Stage 6 draws Simon's trail on its map.");
await stage6.close();
await browser.close();
