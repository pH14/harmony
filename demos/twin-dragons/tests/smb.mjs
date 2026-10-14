// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import smb, { areaRoom, buildMap, buildSprites, MAP_WIDTHS, MAP_HEIGHT } from "../src/games/super-mario-bros.js";
import { createEngine } from "../src/emulator.js";
import { isMapEvidence } from "../src/world.js";
import { snapshotDigest } from "../src/media.js";
import { RoundSearch, ROUND_PATHS } from "../src/rollout.js";
import init, { Explorer } from "../rust/pkg/twin_dragons_browser.js";
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
globalThis.harmonyEngine = engine;
await init({ module_or_path: await readFile(new URL("../rust/pkg/twin_dragons_browser_bg.wasm", import.meta.url)) });

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

const root = engine.boot(0);
for (const seed of [27]) {
  engine.restore(root);
  const search = new Explorer(smb.id, seed),
    rounds = new RoundSearch(search, [engine]);
  let found = null, tries = 0;
  for (; tries < 30000 && !found; tries += ROUND_PATHS) {
    const { batch } = rounds.round();
    for (const point of batch.points)
      if (point.retained !== null && point.observation.cleared >= 1 && !point.observation.loading) found = point;
  }
  assert.ok(found, `Seed ${seed} reaches World 1-2 within 30000 paths`);
  const state = JSON.parse(search.state(found.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(found.retained), "The archived state replays exactly from its controller history");
  const decoded = engine.observation();
  assert.equal(decoded.level, 1);
  for (const key of Object.keys(decoded)) assert.equal(decoded[key], state.observation[key], `Browser and Rust decoders agree on ${key}`);
  console.log(`Seed ${seed}: World 1-2 at ${tries} paths; exact replay passed.`);
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
await page.waitForFunction(() => document.documentElement.dataset.game === "super-mario-bros" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 200, null, { timeout: 60000 });
assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), "World 1-1");
assert.deepEqual(errors, []);
console.log("Choosing the ROM in the game picker stores it and starts a Super Mario Bros. search.");

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
assert.ok(expanded.page <= 0 && expanded.strip <= 0, `The expanded level list wraps: ${JSON.stringify(expanded)}`);
assert.deepEqual(errors, []);
console.log("Cleared levels collapse into one button, and nothing scrolls sideways on desktop or phone.");
await browser.close();
