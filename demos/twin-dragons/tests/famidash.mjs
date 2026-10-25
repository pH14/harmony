// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { chromium } from "@playwright/test";
import famidash, { FAMIDASH } from "../src/games/famidash.js";
import { createEngine } from "../src/emulator.js";
import { snapshotDigest, snapshotHash } from "../src/media.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
import { isMapEvidence } from "../src/world.js";
import { TOUR_KEY } from "../src/tour.js";
import { showOnMap } from "./map-point.mjs";

const base = new URL("../public/", import.meta.url),
  wasmBinary = await readFile(new URL("engine/quicknes.wasm", base)),
  engines = new Map(),
  catalogs = new Map(),
  identity = coreIdentity("browser");
for (const group of FAMIDASH) {
  const engine = await createEngine(base, group, { rom: await readFile(new URL(group.rom.file, base)), wasmBinary });
  engines.set(group.id, engine);
  const catalog = JSON.parse(await readFile(new URL("maps.json", new URL(group.assets, base))));
  catalogs.set(group.id, catalog);
  assert.deepEqual(catalog.maps.map((map) => map.name), group.levels.map((level) => level.label), `${group.group} has one map per level`);
  for (const { id: level, label } of group.levels) {
    engine.boot(level);
    const o = engine.observation(),
      map = catalog.maps.find((entry) => entry.id === level);
    assert.equal(o.level, level);
    assert.equal(o.loading, false, `${label} boots into play`);
    assert.equal(o.cleared, level);
    assert.ok(isMapEvidence(o, catalog.levels));
    assert.ok(o.x < 256 && o.y >= 0 && o.y < map.height, `${label} starts in the first screen of its map (${o.x}, ${o.y})`);
  }
}
const levelCount = FAMIDASH.reduce((sum, group) => sum + group.levels.length, 0);
console.log(`Each of the ${levelCount} levels in ${FAMIDASH.map((group) => group.group).join(", ")} boots into play in the first screen of its map.`);

for (const group of FAMIDASH) {
  const engine = engines.get(group.id),
    worker = campaignWorker(engine, identity),
    { id: level, label } = group.levels[Math.floor(Math.random() * group.levels.length)],
    seed = Math.floor(Math.random() * 2 ** 32),
    root = engine.boot(level),
    search = campaignSearch(engine, identity, { seed, executions: 2000, ...(level ? { root } : {}) });
  let deepest = null;
  while (runCampaign(search, worker, { jobs: 16 }))
    for (const point of JSON.parse(search.take_batch()).points)
      if (point.retained !== null && (!deepest || point.observation.x > deepest.observation.x)) deepest = point;
  assert.ok(deepest?.observation.x > 0, `The search with seed ${seed} moves forward in ${group.group} ${label}`);
  const state = JSON.parse(search.state(deepest.retained));
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(deepest.retained), "The archived state replays exactly from its controller history");
  assert.deepEqual(engine.observation(), state.observation, "The replayed state decodes to the archived observation");
  console.log(`${group.group} ${label} with seed ${seed}: deepest state at x ${deepest.observation.x}, ${deepest.observation.percent}%; exact replay passed.`);
  search.free();
}

const engine = engines.get(famidash.id),
  worker = campaignWorker(engine, identity);
{
  const root = engine.boot(0),
    search = campaignSearch(engine, identity, { seed: 1, executions: 30000 });
  let arrival = null;
  while (!arrival && runCampaign(search, worker, { jobs: 16 }))
    arrival = JSON.parse(search.take_batch()).points.find((point) => point.retained !== null && point.observation.level === 1) ?? null;
  assert.ok(arrival, "A search from Stereo Madness clears it and goes on to Back on Track");
  assert.equal(arrival.observation.cleared, 1);
  const state = JSON.parse(search.state(arrival.retained));
  assert.ok(state.actions.some((action) => action.buttons & 0x80), "The history holds the Right press that picks the next level");
  engine.restore(root);
  for (const action of state.actions) engine.run(action.buttons, action.frames);
  assert.equal(snapshotDigest(engine.capture()), search.digest(arrival.retained), "The state in Back on Track replays exactly from its controller history");
  assert.equal(engine.observation().level, 1);
  console.log(`After ${JSON.parse(search.take_batch()).executions} executions a cleared Stereo Madness advanced to Back on Track, and the history with the target's menu presses replays exactly.`);
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
await page.waitForFunction(() => document.documentElement.dataset.game === "famidash", null, { timeout: 60000 });
await page.locator("#level-grid:not([hidden]) button").first().waitFor({ timeout: 60000 });
assert.deepEqual(await page.locator("#level-grid h3").allInnerTexts(), FAMIDASH.map((group) => group.group.toUpperCase()), "The level selector groups Famidash levels by side");
assert.deepEqual(
  (await page.locator("#level-grid button").allInnerTexts()).map((text) => text.trim()),
  FAMIDASH.flatMap((group) => group.levels.map((level) => level.label)),
);
await page.locator("#level-grid button").first().click();
await page.waitForFunction(() => document.documentElement.dataset.game === "famidash" && Number(document.querySelector("#hero-timelines")?.textContent.replace(/\D/g, "") ?? 0) > 100, null, { timeout: 60000 });
assert.equal(await page.locator('#level-strip [aria-pressed="true"]').textContent(), "Stereo Madness");
assert.match(await page.locator(".game-attribution").innerText(), /^Famidash by TFDSoft, MIT$/);
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

const sides = await browser.newPage({ viewport: { width: 1280, height: 900 } }),
  bSides = FAMIDASH.find((group) => group.id === "famidash-b"),
  pick = bSides.levels[Math.floor(Math.random() * bSides.levels.length)];
sides.on("pageerror", (error) => errors.push(error.message));
await sides.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await sides.goto(`${url}/?game=famidash`);
await sides.locator("#level-picker[open]").waitFor({ timeout: 60000 });
await sides.locator(`#level-grid [data-game="famidash-b"][data-level="${pick.id}"]`).click();
await sides.waitForFunction(() => document.documentElement.dataset.game === "famidash-b" && Number(document.querySelector("#hero-timelines")?.textContent.replace(/\D/g, "") ?? 0) > 100, null, { timeout: 60000 });
assert.equal(await sides.locator('#level-strip [aria-pressed="true"]').textContent(), pick.label);
assert.equal(new URL(sides.url()).searchParams.get("level"), String(pick.id));
await sides.locator("#credits").click();
await sides.locator("#info[open]").waitFor();
await sides.waitForFunction(() => document.querySelector("#info-content").innerText.includes("Xenogenesis: THEFATRAT"));
assert.match(await sides.locator("#info-content").innerText(), /B-Sides[\s\S]*D-Sides/, "The credits list the music of every side");
await sides.locator("#close-info").click();
await sides.locator("#new-search").click();
await sides.locator("#level-picker[open]").waitFor();
assert.equal(await sides.locator(`#level-grid [data-game="famidash-b"][data-level="${pick.id}"]`).getAttribute("aria-current"), "true");
assert.equal(await sides.locator("#homebrew-games button", { hasText: "Famidash" }).getAttribute("aria-pressed"), "true");
assert.deepEqual(errors, []);
await sides.close();
console.log(`Picking the B-Sides level ${pick.label} loads the B-Sides ROM and searches it, and the credits name the music of every side.`);

const dash = await browser.newPage({ viewport: { width: 1280, height: 900 } });
dash.on("pageerror", (error) => errors.push(error.message));
await dash.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await dash.goto(`${url}/?game=famidash&level=21&pace=0`);
await dash.waitForFunction(() => Number(document.querySelector("#hero-timelines")?.textContent.replace(/\D/g, "") ?? 0) > 50, null, { timeout: 60000 });
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

const follow = await browser.newPage({ viewport: { width: 1280, height: 900 } });
follow.on("pageerror", (error) => errors.push(error.message));
await follow.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
await follow.goto(`${url}/?game=famidash&level=0&pace=0&workers=4`);
await follow.waitForFunction(() => document.querySelector('#level-strip [aria-pressed="true"]')?.textContent === "Back on Track", null, { timeout: 900000 });
assert.ok(await follow.locator('.area-map[data-map="1"]').isVisible(), "The main view shows the Back on Track map");
assert.deepEqual(errors, []);
console.log("When the search clears Stereo Madness, the level strip and the main view follow it into Back on Track.");
await browser.close();
