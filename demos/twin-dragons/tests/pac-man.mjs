// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import pacMan, { buildMap, buildSprites } from "../src/games/pac-man.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { validateTape } from "../src/heat.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.PAC_MAN_ROM || `${homedir()}/workspace/roms/pac-man.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Pac-Man: no ROM at ${path}. Set PAC_MAN_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, pacMan, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

for (let level = 0; level < pacMan.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation();
  assert.deepEqual([o.level, o.loading, o.health, o.dots, o.cleared], [level, false, 3, 192, level * 4], `Maze group ${level + 1} boots into play`);
  assert.equal(pacMan.levelsCleared(o), level);
}
console.log("Each of the 6 maze groups boots into play with three lives.");

const root = engine.boot(0),
  start = engine.observation();
engine.run(0x40, 40);
const moved = engine.observation();
assert.ok(moved.x < start.x && moved.dots < 192 && moved.score > 0, "Pac-Man walks left and eats dots");
const rust = JSON.parse((await import("../rust/pkg/twin_dragons_browser.js")).pac_man_observation(engine.memory()));
assert.deepEqual(rust, moved);
console.log("The Rust decoder and the page agree.");

engine.restore(root);
const map = buildMap(engine, 0);
assert.equal(map.data.length, map.width * map.height * 4);
assert.ok(map.data.some((v, i) => i % 4 === 3 && v > 0), "The maze map has pixels");
assert.notEqual(buildMap(engine, 1).data.buffer, map.data.buffer, "Each map call returns its own buffer");
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Pac-Man");
console.log("The maze map and the sprite sheet build from the ROM.");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity);
function furthestFrom(search) {
  let furthest = null;
  while (runCampaign(search, worker, { jobs: 16 }))
    for (const point of JSON.parse(search.take_batch()).points)
      if (point.retained !== null && (!furthest || point.observation.cleared * 192 + 192 - point.observation.dots > furthest.observation.cleared * 192 + 192 - furthest.observation.dots)) furthest = point;
  assert.ok(furthest, "The search retains states in the maze");
  return furthest;
}
function replays(search, point, from) {
  const state = JSON.parse(search.state(point.retained));
  engine.restore(from);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(point.retained), "The archived state replays exactly from its controller history");
  assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
  return state;
}
engine.restore(root);
const search = campaignSearch(engine, identity, { seed: 3, executions: 1500 }),
  best = furthestFrom(search);
replays(search, best, root);
assert.ok(best.observation.dots < 192, "The search eats dots");
console.log(`Pac-Man: the search leaves ${best.observation.dots} dots; exact replay passed.`);
search.free();

const history = [{ buttons: 0x40, frames: 30 }, { buttons: 0x10, frames: 20 }, { buttons: 0x40, frames: 20 }];
assert.equal(validateTape({ format: "harmony-pac-man-browser-v1", rom_sha256: pacMan.rom.sha256, core_revision: engine.revision, endpoint_sha256: "0".repeat(64), actions: history }, pacMan, engine.revision), 70);
console.log("A history of direction presses is a valid branch point.");

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=pac-man&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
assert.equal(await page.locator("#rom-title").innerText(), "To explore Pac-Man please supply the game’s ROM");
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 60000 });
await page.mouse.move(2, 890);
await page.waitForFunction(() => document.documentElement.dataset.game === "pac-man" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts a Pac-Man search.");
await browser.close();
