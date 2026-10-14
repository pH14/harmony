// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { chromium } from "@playwright/test";
import famidash, { LEVEL_NAMES } from "../src/games/famidash.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest, snapshotHash } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { isMapEvidence } from "../src/world.js";
import { TOUR_KEY } from "../src/tour.js";
import { showOnMap } from "./map-point.mjs";

const base = new URL("../public/", import.meta.url),
  assets = new URL(famidash.assets, base);
const engine = await createEngine(base, famidash, {
  rom: await readFile(new URL(famidash.rom.file, base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const catalog = JSON.parse(await readFile(new URL("maps.json", assets)));
assert.deepEqual(catalog.maps.map((map) => map.name), LEVEL_NAMES);

for (let level = 0; level < famidash.levelCount; level++) {
  engine.boot(level);
  const o = engine.observation(), map = catalog.maps[level];
  assert.equal(o.level, level);
  assert.equal(o.loading, false, `${LEVEL_NAMES[level]} boots into play`);
  assert.equal(o.cleared, level);
  assert.ok(isMapEvidence(o, catalog.levels));
  assert.ok(o.x < 256 && o.y > map.height - 240 && o.y < map.height, `${LEVEL_NAMES[level]} starts at the bottom left of its map (${o.x}, ${o.y})`);
}
console.log("Each of the 22 main levels boots into play at the bottom left of its map.");

const identity = coreIdentity("browser"),
  worker = campaignWorker(engine, identity);
for (const level of [0, 13]) {
  const root = engine.boot(level),
    search = campaignSearch(engine, identity, { seed: 1, executions: 2000, ...(level ? { root } : {}) });
  let deepest = null;
  while (runCampaign(search, worker, { jobs: 16 }))
    for (const point of JSON.parse(search.take_batch()).points)
      if (point.retained !== null && (!deepest || point.observation.percent > deepest.observation.percent)) deepest = point;
  assert.ok(deepest?.observation.percent > 0, `The search makes progress in ${LEVEL_NAMES[level]}`);
  const state = JSON.parse(search.state(deepest.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(deepest.retained), "The archived state replays exactly from its controller history");
  assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
  console.log(`${LEVEL_NAMES[level]}: deepest state at ${deepest.observation.percent}%; exact replay passed.`);
  search.free();
}

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  errors = [];
page.on("pageerror", (error) => errors.push(error.message));
page.on("response", (response) => {
  if (response.status() >= 400) errors.push(`${response.status()} ${response.url()}`);
});
await page.addInitScript((key) => {
  localStorage.setItem(key, "seen");
  const Base = window.Worker;
  window.Worker = class extends Base {
    constructor(...args) {
      super(...args);
      this.addEventListener("message", ({ data }) => {
        if (data.type === "ready") window.famidashRoot = data.state;
      });
    }
    postMessage(data, ...rest) {
      if (data.type === "fork") window.famidashFork = structuredClone(data);
      super.postMessage(data, ...rest);
    }
  };
}, TOUR_KEY);
const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
await page.goto(`${url}/`);
await page.locator("#level-picker[open]").waitFor({ timeout: 60000 });
const homebrew = await page.locator("#homebrew-games button").allInnerTexts();
assert.deepEqual(homebrew.slice(0, 3), ["Twin Dragons", "Famidash", "Tobu Tobu Girl Deluxe"], "Famidash sits between Twin Dragons and Tobu Tobu Girl");
await page.locator("#homebrew-games button", { hasText: "Famidash" }).click();
await page.locator("#level-grid:not([hidden]) button").first().waitFor({ timeout: 60000 });
assert.equal(await page.locator("html").getAttribute("data-game"), "famidash");
assert.deepEqual((await page.locator("#level-grid button").allInnerTexts()).map((text) => text.trim()), LEVEL_NAMES);
await page.locator("#level-grid button").first().click();
await page.waitForFunction(() => document.documentElement.dataset.game === "famidash" && Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 100, null, { timeout: 60000 });
assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), "Stereo Madness");
assert.match(await page.locator(".game-attribution").innerText(), /unofficial fan demake of RobTop’s Geometry Dash/);
const progress = async () => {
  const fields = await page.locator("#details div").allInnerTexts();
  const field = fields.find((text) => text.startsWith("Level progress"));
  return field ? Number(field.match(/(\d+)%/)[1]) : -1;
};
await page.waitForFunction(() => [...document.querySelectorAll("#details div")].some((div) => /Level progress\s*([1-9]\d*)%/.test(div.innerText)), null, { timeout: 120000 });
const reached = await progress();
assert.ok(reached > 0, `The followed state shows progress in Stereo Madness (${reached}%)`);
console.log(`The Famidash picker entry starts a Stereo Madness search, and the followed state shows ${reached}% progress.`);

await page.locator("#pause").click();
await page.waitForFunction(() => document.querySelector(".map-wrap")?.dataset.camera !== "focus", null, { timeout: 10000 });
const { x, y, level } = await page.evaluate(() => window.famidashRoot.observation);
const point = await showOnMap(page, level, Math.floor((x + 8) / 32) * 32 + 16, Math.floor(y / 32) * 32 + 24);
await page.mouse.click(point.x, point.y);
await page.waitForFunction(() => !document.querySelector("#inspector").hidden && ["Exact replay ✓", "Original game"].includes(document.querySelector("#verification").textContent) && !document.querySelector("#take-control").disabled, null, { timeout: 60000 });
console.log(`Selecting the cell at ${x}, ${y} opens it in the inspector.`);

await page.locator("#take-control").click();
await page.keyboard.down("Space");
await page.waitForTimeout(120);
await page.keyboard.up("Space");
await page.waitForTimeout(300);
await page.locator("#search-here").click();
await page.waitForFunction(() => !!window.famidashFork, null, { timeout: 30000 });
const tape = await page.evaluate(() => window.famidashFork.tape);
assert.ok(tape.actions.some((action) => action.buttons & 1), "The branch history holds the human jump");
engine.restore(engine.boot(0));
for (const action of tape.actions) engine.run(action.buttons, action.frames);
assert.equal(await snapshotHash(engine.capture()), tape.endpoint_sha256, "Human inputs reproduce the exact endpoint passed to the worker");
await page.waitForFunction(() => document.querySelector("#branch-tree button[aria-pressed=true]")?.dataset.search === "1", null, { timeout: 30000 });
await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replace(/\D/g, "")) > 0, null, { timeout: 60000 });
assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, "Nothing scrolls sideways");
assert.deepEqual(errors, []);
console.log("Playing from the selected state and branching starts a search whose root replays exactly.");

const dash = await browser.newPage({ viewport: { width: 1280, height: 900 } });
dash.on("pageerror", (error) => errors.push(error.message));
await dash.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await dash.goto(`${url}/?game=famidash&level=21&pace=0`);
await dash.waitForFunction(() => Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 50, null, { timeout: 60000 });
await dash.locator("#pause").click();
const dashView = dash.locator('.area-map[data-map="21"]'), dashPart = dash.locator('.map-row[data-map="21"] .room-part');
const dashStrip = await dash.locator('.level-overview[data-map="21"]').boundingBox();
const centerY = async () => Number(await dashView.getAttribute("data-center-y"));
await dash.mouse.click(dashStrip.x + 4, dashStrip.y + 1);
const top = await centerY();
await dash.mouse.click(dashStrip.x + 4, dashStrip.y + dashStrip.height - 1);
assert.ok((await centerY()) > top + 8, "Clicking the bottom of the level strip moves the view down");
for (const dir of [-1, 1]) {
  await dash.waitForFunction((key) => document.querySelector('.map-row[data-map="21"] .room-part').dataset[key] === "true", dir > 0 ? "panDown" : "panUp");
  const before = await centerY();
  await dashView.hover();
  await dashPart.locator(`.pan-edge[data-axis="y"][data-dir="${dir}"]`).hover();
  await dash.waitForFunction(([dir, before]) => dir * (Number(document.querySelector('.area-map[data-map="21"]').dataset.centerY) - before) > 8, [dir, before]);
  await dash.mouse.move(2, 890);
}
assert.deepEqual(errors, []);
console.log("On Dash, the level strip and the top and bottom pan edges move the view up and down.");
await browser.close();
