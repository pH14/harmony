// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { createEngine } from "../src/emulator.js";
import zelda, { buildMap, buildSprites, MAP_SIZES } from "../src/games/zelda.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { snapshotDigest } from "../src/media.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";
import { showOnMap } from "./map-point.mjs";

const path = process.env.ZELDA_ROM || "/Users/phemberger/workspace/harmony/.arcade-local/roms/zelda.nes";
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Zelda: no ROM at ${path}. Set ZELDA_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, zelda, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

const root = engine.boot(0);
const start = engine.observation();
assert.deepEqual([start.level, start.mode, start.room, start.cleared], [0, 5, 0x77, 0], "A new game starts in the overworld at room $77 with no Triforce");
assert.equal(start.containers, 3, "The new game has three heart containers");
console.log("Zelda boots into the overworld at room $77.");

const overworld = buildMap(engine, 0);
assert.equal(overworld.tiles.length, 128, "The overworld has 128 screens");
for (const tile of overworld.tiles) {
  assert.ok(tile.x + tile.width <= overworld.width && tile.y + tile.height <= overworld.height);
  assert.equal(tile.data.length, tile.width * tile.height * 4);
}
const dungeon = buildMap(engine, 1);
assert.equal(dungeon.tiles.length, 17, "Dungeon 1 has 17 rooms on its 16 by 8 grid");
assert.deepEqual(MAP_SIZES[1], { width: 4096, height: 1344 });
engine.restore(root);
console.log("Overworld and dungeon 1 maps build from the ROM.");

const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Link");
engine.restore(root);
console.log("The Link sprite sheet builds from the ROM.");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity),
  search = campaignSearch(engine, identity, { seed: 3, executions: 300 });
while (runCampaign(search, worker, { jobs: 16 }));
const points = JSON.parse(search.take_batch()).points;
assert.ok(points.length > 0, "The search reports points");
const retained = points.find((point) => point.retained !== null);
assert.ok(retained, "The search retains a state");
const state = JSON.parse(search.state(retained.retained));
engine.restore(root);
for (const action of state.actions) engine.run(action.buttons, action.frames);
assert.equal(snapshotDigest(engine.capture()), search.digest(retained.retained), "The archived state replays exactly from its controller history");
assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
search.free();
console.log("A short overworld search retains states that replay exactly.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => {
  localStorage.setItem(key, "seen");
  const WorkerClass = window.Worker;
  window.Worker = class extends WorkerClass {
    constructor(...args) {
      super(...args);
      this.addEventListener("message", ({ data }) => {
        if (data.type !== "batch") return;
        for (const point of data.points)
          if (point.retained !== null && !point.observation.loading && point.observation.health > 0)
            (window.tdStates ??= []).push(point.observation);
      });
    }
  };
}, TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=zelda&pace=0&workers=1`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 120000 });
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "zelda" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 20, null, { timeout: 120000 });
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts a Zelda search with the overworld map.");

await page.waitForFunction(() => (window.tdStates?.length ?? 0) >= 60, null, { timeout: 120000 });
const states = await page.evaluate(() => {
  const unique = new Map(window.tdStates.map((o) => [`${o.level}:${o.x}:${o.y}`, o]));
  return [...unique.values()].map((o) => ({ level: o.level, x: o.x, y: o.y }));
});
const edge = states.find((o) => ((o.y % 32) + 32) % 32 < 8), inner = states.find((o) => ((o.y % 32) + 32) % 32 >= 16);
for (const state of [edge, inner].filter(Boolean)) {
  await page.evaluate(() => { document.querySelector("#inspector").dataset.empty = "unset"; });
  const at = await showOnMap(page, state.level, state.x + 8, state.y);
  await page.mouse.click(at.x, at.y);
  await page.waitForFunction(() => document.querySelector("#inspector").dataset.empty !== "unset", null, { timeout: 15000 });
  assert.equal(await page.evaluate(() => document.querySelector("#inspector").dataset.empty), "false", `Clicking the Zelda state at ${state.x},${state.y} selects its cell`);
}
console.log("Clicking a Zelda trail point selects the cell it sits in.");
await browser.close();
