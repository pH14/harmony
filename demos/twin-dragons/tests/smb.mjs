// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import smb, { areaRoom, buildMap, buildSprites, MAP_WIDTHS, MAP_HEIGHT } from "../src/games/super-mario-bros.js";
import { createEngine } from "../src/emulator.js";
import { isMapEvidence } from "../src/world.js";
import { snapshotDigest } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const path = process.env.SMB_ROM || `${homedir()}/workspace/roms/Super Mario Bros. (World).nes`;
let rom;
try {
  rom = new Uint8Array(await readFile(path));
} catch {
  console.log(`Skipping Super Mario Bros.: no ROM at ${path}. Set SMB_ROM to run it.`);
  process.exit(0);
}
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, smb, { rom, wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)) });

for (let level = 0; level < smb.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation();
  assert.equal(o.level, level);
  assert.equal(o.loading, false, `World ${(level >> 2) + 1}-${(level & 3) + 1} boots onto its map`);
  assert.equal(o.health, 1);
  assert.ok(isMapEvidence(o, smb.catalog.levels));
  assert.ok(o.x < MAP_WIDTHS[level] && o.y < MAP_HEIGHT);
}
console.log("Each of the 32 levels boots onto its own map.");

for (const level of [0, 1, 3, 31]) {
  const map = buildMap(engine, level);
  assert.equal(map.reached, MAP_WIDTHS[level], `Level ${level} map reaches its recorded width`);
  assert.equal(map.data.length, MAP_WIDTHS[level] * MAP_HEIGHT * 4);
}
const water = buildMap(engine, areaRoom(31, 2));
assert.equal(water.width, 1152, "The World 8-4 underwater area gets its own map");
const sheet = buildSprites(engine);
assert.ok(sheet.data.some((v, i) => i % 4 === 3 && v > 0), "The sprite sheet holds Mario");
console.log("Level maps and the sprite sheet build from the ROM.");

for (const room of [areaRoom(1, 41), areaRoom(5, 41)]) {
  const intro = buildMap(engine, room);
  assert.ok(intro.reached > 0, `The intro scene ${smb.areaMap(room).label} builds a map`);
}
engine.boot(13);
const warping = engine.memory();
warping[0x75f] = 4;
warping[0x75c] = 0;
warping[0x0e] = 3;
assert.equal(smb.decode(warping).loading, true, "A pipe entry with the warp destination already written counts as loading");
console.log("Intro scenes build, and a warp pipe entry reads as loading.");

const root = engine.boot(0),
  identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity);
for (const seed of [27]) {
  const search = campaignSearch(engine, identity, { seed, executions: 30000 });
  let found = null;
  while (!found && runCampaign(search, worker, { jobs: 16 }))
    for (const point of JSON.parse(search.take_batch()).points)
      if (point.retained !== null && point.observation.stage === 1 && !point.observation.loading) found = point;
  assert.ok(found, `Seed ${seed} reaches World 1-2 within 30000 executions`);
  const executions = JSON.parse(search.take_batch()).executions;
  const state = JSON.parse(search.state(found.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(found.retained), "The archived state replays exactly from its controller history");
  assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
  assert.deepEqual([state.observation.world, state.observation.stage], [0, 1]);
  console.log(`Seed ${seed}: World 1-2 at ${executions} executions; exact replay passed.`);
  search.free();
}

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=super-mario-bros&pace=0&workers=2`);
await page.locator("#level-picker[open]").waitFor({ timeout: 30000 });
assert.equal(await page.locator("#rom-title").innerText(), "To explore Super Mario Bros. please supply the game’s ROM");
assert.equal(await page.locator("#playthrough").isVisible(), true, "The ROM page offers a playthrough video");
await page.locator("#rom-drop").dispatchEvent("drop", { dataTransfer: await page.evaluateHandle(() => { const t = new DataTransfer(); t.items.add(new File([new Uint8Array(16)], "wrong.nes")); return t; }) });
await page.locator("#rom-message:not([hidden])").waitFor();
assert.match(await page.locator("#rom-message").innerText(), /not the Super Mario Bros\. ROM/, "A dropped file that is not the ROM is refused");
assert.equal(await page.locator("html").getAttribute("data-game"), "twin-dragons", "Without a stored ROM the page shows Twin Dragons");
const chooser = page.waitForEvent("filechooser");
await page.locator("#game-choice button", { hasText: "Super Mario Bros." }).click();
await page.locator("#rom-drop").click();
await (await chooser).setFiles(path);
await page.locator("#level-grid:not([hidden]) button").first().waitFor({ timeout: 60000 });
assert.equal(await page.locator("html").getAttribute("data-game"), "super-mario-bros");
assert.equal(await page.locator("#hero-timelines").innerText(), "0", "The level picker waits for a level before searching");
assert.equal(await page.locator("#level-grid button").first().innerText().then((t) => t.split(/\s·|\n/)[0]), "World 1-1");
await page.locator("#level-grid button").first().click();
await page.waitForFunction(() => document.documentElement.dataset.game === "super-mario-bros" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), "World 1-1");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker stores it and starts a Super Mario Bros. search.");

await page.waitForFunction(() => document.querySelectorAll("#room-tabs button").length >= 2, null, { timeout: 300000 });
const tab = page.locator('#room-tabs button:not([aria-pressed="true"])').first(),
  pinnedRoom = await tab.getAttribute("data-map"),
  pinnedLabel = await tab.innerText();
await tab.click();
await page.waitForTimeout(4000);
const shown = await page.evaluate(() => ({ room: document.querySelector('#room-tabs [aria-pressed="true"]')?.dataset.map, label: document.querySelector("#map-label").textContent }));
assert.ok(shown.room === pinnedRoom || !shown.label.toLowerCase().startsWith(pinnedLabel.split(" ").slice(0, 2).join(" ").toLowerCase()), `A clicked room tab holds the view while the search stays in that level: ${JSON.stringify(shown)}`);
await page.locator('#level-strip [aria-pressed="true"]').click();
console.log("A clicked room tab keeps the view on that room.");

await page.mouse.move(2, 2);
await page.locator("#level-strip .level-more").waitFor({ timeout: 300000 });
const layout = () => page.evaluate(() => {
  const strip = document.querySelector("#level-strip"), tabs = document.querySelector("#room-tabs");
  return {
    page: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    strip: strip.scrollWidth - strip.clientWidth,
    tabs: tabs.scrollWidth - tabs.clientWidth,
    rows: document.querySelectorAll(".map-row").length,
    levels: strip.querySelectorAll("[data-level]").length,
  };
});
for (const width of [1280, 390]) {
  await page.setViewportSize({ width, height: 900 });
  await page.waitForTimeout(300);
  const collapsed = await layout();
  assert.ok(collapsed.page <= 0 && collapsed.strip <= 0 && collapsed.tabs <= 0, `At ${width} px nothing scrolls sideways: ${JSON.stringify(collapsed)}`);
  assert.equal(collapsed.rows, 1, "Only the room on screen has a map row");
  assert.ok(collapsed.levels <= 3, "Earlier levels collapse behind one button");
}
await page.locator("#level-strip .level-more").click();
const expanded = await layout();
assert.equal(await page.locator("#level-strip .level-more").getAttribute("aria-expanded"), "true");
assert.ok(expanded.levels >= 4, "Expanding the level list shows the earlier levels");
const route = await page.locator("#level-strip [data-level]").evaluateAll((buttons) => buttons.map((b) => Number(b.dataset.level)));
assert.deepEqual(route, [...new Set(route)].sort((a, b) => a - b), `The level row lists the followed path once each, in order: ${route}`);
assert.ok(expanded.page <= 0 && expanded.strip <= 0, `The expanded level list wraps: ${JSON.stringify(expanded)}`);
assert.deepEqual(errors, []);
console.log("Cleared levels collapse into one button, and nothing scrolls sideways on desktop or phone.");
await browser.close();
