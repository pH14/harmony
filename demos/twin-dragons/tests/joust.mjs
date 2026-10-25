// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import joust, { buildMap, buildSprites } from "../src/games/joust.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.JOUST_ROM || `${homedir()}/workspace/roms/Joust (USA).nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Joust: no ROM at ${path}. Set JOUST_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, joust, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

const root = engine.boot(0);
const start = engine.observation();
assert.deepEqual([start.level, start.loading, start.cleared], [0, false, 0], "A new game starts in wave 1");
assert.equal(start.health, 5, "A new game starts with four lives");
console.log("Joust boots into wave 1.");

for (const level of [0, 4, 11]) {
  const map = buildMap(engine, level);
  assert.equal(map.data.length, 256 * 224 * 4);
  assert.ok(map.data.some((v, i) => i % 4 !== 3 && v > 0), `Wave ${level + 1} has an arena`);
}
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds the knight");
console.log("Arena maps and the sprite sheet build from the ROM.");

for (const level of [1, 9, 24, 48]) {
  engine.boot(level);
  const at = engine.observation();
  assert.deepEqual([at.level, at.loading, at.wave, at.lives], [level, false, level + 1, 4], `Starting at wave ${level + 1} boots into that wave`);
}
console.log("A search can start at any wave.");
engine.boot(0);

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity),
  search = campaignSearch(engine, identity, { seed: 3, executions: 400 });
let retained = null;
while (runCampaign(search, worker, { jobs: 16 }))
  for (const point of JSON.parse(search.take_batch()).points) if (point.retained !== null) retained = point;
assert.ok(retained, "The search retains states in wave 1");
const state = JSON.parse(search.state(retained.retained));
engine.restore(root);
for (const action of state.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), search.digest(retained.retained), "The archived state replays exactly from its controller history");
console.log("An archived state replays exactly from power-on.");
search.free();

const late = campaignSearch(engine, identity, { seed: 4, executions: 200, root: engine.boot(9) });
let latest = null;
while (runCampaign(late, worker, { jobs: 16 }))
  for (const point of JSON.parse(late.take_batch()).points) if (point.retained !== null) latest = point;
assert.ok(latest && latest.observation.wave >= 10, "A search rooted at wave 10 keeps states in wave 10");
late.free();
console.log("A search rooted at a later wave retains states there.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=joust&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 60000 });
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "joust" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts a Joust search.");
await browser.close();
