// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import mm2, { buildMap, buildSprites, layout, MAP_SIZES, ORDER } from "../src/games/mega-man-2.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { validateTape } from "../src/heat.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.MM2_ROM || `${homedir()}/workspace/roms/mm2.nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Mega Man 2: no ROM at ${path}. Set MM2_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, mm2, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

for (let level = 0; level < mm2.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation();
  assert.equal(o.level, level);
  assert.equal(o.loading, false, `${mm2.catalog.levels[level].label} boots into play`);
  assert.equal(o.health, 28);
  assert.equal(o.cleared, level);
  assert.ok(o.x < MAP_SIZES[level].width && o.y < MAP_SIZES[level].height);
}
console.log("Each of the 14 stages boots into play on its own map.");

for (const level of [0, 5, 7]) {
  const map = buildMap(engine, level);
  assert.equal(map.reached, layout(ORDER[level]).rooms.length - 1, `${mm2.catalog.levels[level].label} map reaches the boss room`);
  assert.equal(map.data.length, MAP_SIZES[level].width * MAP_SIZES[level].height * 4);
  assert.ok(map.data.some((v, i) => i % 4 === 3 && v > 0), `${mm2.catalog.levels[level].label} map has pixels`);
}
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Mega Man");
console.log("Stage maps and the sprite sheet build from the ROM.");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity);
for (const level of [0, ORDER.indexOf(8)]) {
  const root = engine.boot(level),
    search = campaignSearch(engine, identity, { seed: 3, executions: 3000, ...(level ? { root } : {}) });
  let deepest = null;
  while (runCampaign(search, worker, { jobs: 16 }))
    for (const point of JSON.parse(search.take_batch()).points)
      if (point.retained !== null && (!deepest || point.observation.climb > deepest.observation.climb)) deepest = point;
  const name = mm2.catalog.levels[level].label;
  assert.ok(deepest, `The search retains states in ${name}`);
  const state = JSON.parse(search.state(deepest.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(deepest.retained), "The archived state replays exactly from its controller history");
  assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
  console.log(`${name}: deepest state at screen climb ${deepest.observation.climb}; exact replay passed.`);
  search.free();
}

{
  const root = engine.boot(0),
    menu = [{ buttons: 0x80, frames: 30 }, { buttons: 8, frames: 4 }, { buttons: 0, frames: 60 }, { buttons: 8, frames: 4 }, { buttons: 0, frames: 90 }];
  for (const action of menu) engine.run(action.buttons, action.frames);
  assert.equal(
    validateTape({ format: "harmony-mega-man-2-browser-v1", rom_sha256: mm2.rom.sha256, core_revision: engine.revision, endpoint_sha256: "0".repeat(64), actions: menu }, mm2, engine.revision),
    188,
    "A history that opens the weapon menu is a valid branch point",
  );
  const branch = campaignSearch(engine, identity, { seed: 5, executions: 300, root: engine.capture(), prefix: menu });
  let point = null;
  while (runCampaign(branch, worker, { jobs: 16 }))
    for (const p of JSON.parse(branch.take_batch()).points) if (p.retained !== null) point = p;
  const state = JSON.parse(branch.state(point.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), branch.digest(point.retained), "A state found after the branch replays exactly from the stage start");
  assert.deepEqual(state.actions.slice(0, menu.length), menu, "The branch keeps the menu history as its prefix");
  console.log("A history that presses Start branches, and its states replay exactly from the stage start.");
  branch.free();
}

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=mega-man-2&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
assert.equal(await page.locator("#rom-title").innerText(), "To explore Mega Man 2 please supply the game’s ROM");
assert.equal(await page.locator("#playthrough").isVisible(), true, "The ROM page offers a playthrough video");
const chooser = page.waitForEvent("filechooser");
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().click({ timeout: 60000 });
const drawn = await (await page.waitForFunction(() => {
  const strip = document.querySelector('.level-overview[data-map="0"]');
  if (document.documentElement.dataset.game !== "mega-man-2" || !strip || !(Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 0)) return null;
  const data = strip.getContext("2d").getImageData(0, 0, strip.width, strip.height).data, [r, g, b] = data;
  let differ = 0;
  for (let i = 0; i < data.length; i += 4) if (Math.abs(data[i] - r) + Math.abs(data[i + 1] - g) + Math.abs(data[i + 2] - b) > 24) differ++;
  return differ / (data.length / 4);
}, null, { timeout: 60000, polling: "raf" })).jsonValue();
assert.ok(drawn > 0.05, `The stage map is drawn when the first rollouts appear (${(drawn * 100).toFixed(1)}% of the strip)`);
await page.waitForFunction(() => document.documentElement.dataset.game === "mega-man-2" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), "Crash Man");
const strip = page.locator('.level-overview[data-map="0"]'), view = page.locator('.area-map[data-map="0"]');
const stripBox = await strip.boundingBox(), viewBox = await view.boundingBox();
assert.ok(stripBox.x >= viewBox.x + viewBox.width, "The stage map sits beside the game view");
const center = async () => [Number(await view.getAttribute("data-center-x")), Number(await view.getAttribute("data-center-y"))];
await page.mouse.click(stripBox.x + stripBox.width * 0.2, stripBox.y + stripBox.height * 0.9);
const low = await center();
await page.mouse.click(stripBox.x + stripBox.width * 0.8, stripBox.y + stripBox.height * 0.1);
const high = await center();
assert.ok(high[0] > low[0] && high[1] < low[1], `Clicking the stage map moves the view across and up (${low} to ${high})`);
const part = page.locator('.map-row[data-map="0"] .room-part');
for (const dir of [1, -1]) {
  await page.waitForFunction((key) => document.querySelector('.map-row[data-map="0"] .room-part').dataset[key] === "true", dir > 0 ? "panDown" : "panUp");
  const before = (await center())[1];
  await view.hover();
  await part.locator(`.pan-edge[data-axis="y"][data-dir="${dir}"]`).hover();
  await page.waitForFunction(([dir, before]) => dir * (Number(document.querySelector('.area-map[data-map="0"]').dataset.centerY) - before) > 8, [dir, before]);
  await page.mouse.move(2, 890);
}
for (const edge of await part.locator(".pan-edge").all()) {
  const box = await edge.boundingBox();
  assert.ok(box.x + box.width <= stripBox.x, "The pan edges stay off the stage map");
}
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker starts a Mega Man 2 search, and the stage map and the top and bottom pan edges move the view.");
await browser.close();
