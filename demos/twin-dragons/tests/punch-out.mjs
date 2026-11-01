// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import punchOut, { OPPONENTS, buildMap, buildSprites, motion } from "../src/games/punch-out.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.PUNCHOUT_ROM || `${homedir()}/workspace/harmony/.arcade-local/roms/punch-out.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Punch-Out!!: no ROM at ${path}. Set PUNCHOUT_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, punchOut, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

const root = engine.boot(0);
const start = engine.observation();
assert.deepEqual([start.mode, start.opponent, start.round, start.mac_health, start.opponent_health, start.hearts], [1, 0, 1, 96, 96, 20], "Glass Joe's first round starts at full health");
console.log("Punch-Out!! boots into round 1 against Glass Joe.");

for (let level = 0; level < OPPONENTS.length; level++) {
  const map = buildMap(engine, level), observed = engine.observation();
  assert.deepEqual([map.width, map.height, map.data.length], [256, 224, 256 * 224 * 4], `${OPPONENTS[level]} has a 256 by 224 ring`);
  assert.ok(map.data.some((v, i) => i % 4 === 2 && v > 128), `${OPPONENTS[level]} ring shows the blue canvas`);
  assert.deepEqual([observed.opponent, observed.cleared, observed.loading], [level, level, false], `${OPPONENTS[level]} fight is live and counts the opponents already beaten`);
}
console.log(`All ${OPPONENTS.length} opponents boot into a live fight with a ring map.`);

const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds the marker");

engine.boot(4);
const fight = engine.observation(), point = motion(engine.ramView());
assert.equal(point[0], 4, "The motion sample names the fight's opponent");
assert.ok(point[1] < 256 && point[2] < 224, "The motion sample lies on the ring map");
assert.deepEqual(engine.observation(), fight, "Reading motion does not change the game");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity),
  search = campaignSearch(engine, identity, { seed: 3, executions: 300 });
while (runCampaign(search, worker, { jobs: 16 }));
const batch = JSON.parse(search.take_batch());
const retained = batch.points.find((p) => p.retained !== null);
assert.ok(retained, "The search retains fight states from Glass Joe's round");
const state = JSON.parse(search.state(retained.retained));
engine.restore(root);
for (const action of state.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), search.digest(retained.retained), "A retained state replays exactly from its button history");
assert.deepEqual(Object.fromEntries(Object.keys(state.observation).map((key) => [key, engine.observation()[key]])), state.observation, "The replayed state decodes to the archived observation");
search.free();
console.log("A short search retains fight states and they replay exactly.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=punch-out&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().waitFor({ timeout: 60000 });
assert.equal(await page.locator("#level-grid:not([hidden]) button.level-card").count(), OPPONENTS.length, "The picker lists every opponent");
await page.locator("#level-grid:not([hidden]) button").first().click();
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "punch-out" && document.querySelectorAll("#level-picker[open]").length === 0, null, { timeout: 60000 });
await page.waitForTimeout(3000);
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the picker starts a Punch-Out!! search with one card per opponent.");
await browser.close();
