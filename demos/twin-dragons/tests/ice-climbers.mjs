// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import iceClimbers, { buildMap, buildSprites, MAP_HEIGHT, MAP_WIDTH } from "../src/games/ice-climbers.js";
import { createEngine } from "../src/emulator.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.ICE_CLIMBERS_ROM || "/Users/phemberger/workspace/harmony/.arcade-local/roms/ice-climbers.nes";
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Ice Climber: no ROM at ${path}. Set ICE_CLIMBERS_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, iceClimbers, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

engine.boot(0);
const start = engine.observation();
assert.deepEqual([start.level, start.health, start.cleared], [0, 3, 0], "A new game starts on mountain 1 with three lives");
console.log("Ice Climber boots onto mountain 1.");

engine.boot(4);
assert.equal(engine.observation().level, 4, "Poking the mountain byte at the title screen starts that mountain");

const map = buildMap(engine, 0);
assert.equal(map.width, MAP_WIDTH);
assert.equal(map.height, MAP_HEIGHT);
let painted = 0;
for (let i = 3; i < map.data.length; i += 4) if (map.data[i] > 0) painted++;
assert.ok(painted > map.width * map.height * 0.9, "The mountain map is painted from top to bottom");
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds the climber");
console.log("The mountain map and the sprite sheet build from the ROM.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=ice-climbers&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 60000 });
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "ice-climbers" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts an Ice Climber search.");
await browser.close();
