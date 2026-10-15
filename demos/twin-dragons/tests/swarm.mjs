// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import twinDragons from "../src/games/twin-dragons.js";
import { chromium } from "@playwright/test";
import { readFile, mkdir } from "node:fs/promises";
import { createEngine } from "../src/emulator.js";
import { NO_ROOM, MOTION_STRIDE } from "../src/swarm.js";
import { Heatmap, cellKey } from "../src/heat.js";
import { project, isMapEvidence } from "../src/world.js";
import { TOUR_KEY } from "../src/tour.js";
import { campaignSearch, campaignWorker, coreIdentity, runCampaign } from "../src/campaign.js";
const setView=async(page,mode)=>{const want=mode==='both'?['movement','heat']:[mode];const b=v=>page.locator(`[data-viz="${v}"]`);for(const v of want)if(await b(v).getAttribute('aria-pressed')!=='true')await b(v).click();for(const v of ['movement','heat'])if(!want.includes(v)&&await b(v).getAttribute('aria-pressed')==='true')await b(v).click();};
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, twinDragons, {
  rom: await readFile(new URL("twin-dragons.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
const identity = coreIdentity("browser"), worker = campaignWorker(engine, identity);
const root = engine.boot(), catalog = JSON.parse(await readFile(new URL("maps.json", base)));
const mapCatalog = new Map(catalog.maps.map(m => [m.id, m]));
const JOBS = 256;
const search = campaignSearch(engine, identity, { seed: 2 });
runCampaign(search, worker, { jobs: JOBS });
const { points, motion: trails } = JSON.parse(search.take_batch());
assert.ok(trails.length >= JOBS / 2, "Most rollouts must record a sampled trail");
assert.ok(trails.every((t) => t.length % MOTION_STRIDE === 0 && t[0] === 0), "Trails start at the rollout's first frame");
search.free();
const sampledHeat = new Heatmap(), endpointKeys = new Set();
sampledHeat.visitMotion(trails, mapCatalog, 0);
for (const o of points.map((p) => p.observation)) {
  const map = mapCatalog.get(o.level);
  if (map && isMapEvidence(o, catalog.levels)) endpointKeys.add(cellKey(project(o, map)));
}
const transitCells = [...sampledHeat.cells.values()].filter(c => !endpointKeys.has(c.key));
assert.ok(transitCells.length > 0, "Real attempts must mark crossed cells missing from action endpoints");
assert.ok(transitCells.every(c => c.heat > 0 && c.ids.length === 0), "Transit cells must not invent retained histories");
console.log(`Sampled heat: ${transitCells.length} real transit cells missing from endpoint-only heat, without fabricated states.`);
const tape = JSON.parse(await readFile(new URL("fixtures/level-one.json", import.meta.url)));
engine.restore(root);
for (const a of tape.actions) engine.run(a.buttons, a.frames);
const near = campaignSearch(engine, identity, { seed: 5, root: engine.capture(), prefix: tape.actions });
let crossing = null, jobs = 0;
while (!crossing && jobs++ < 2000 && runCampaign(near, worker, { jobs: 1 })) {
  const { motion, finishes } = JSON.parse(near.take_batch());
  for (const index of finishes) {
    const rooms = new Set(motion[index].filter((_, k) => k % MOTION_STRIDE === 1));
    if (rooms.has(0)) crossing = rooms;
  }
}
near.free();
assert.ok(crossing?.has(NO_ROOM), "The rollout that clears Level 1 must be sampled with its loading frames hidden");
console.log(`Swarm sampling: ${trails.length} sampled rollouts, ${trails.reduce((n, t) => n + t.length / MOTION_STRIDE, 0)} poses; the Level 1 clear after ${jobs} rollouts hides its loading frames.`);

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const stripHasSprites = () => [...document.querySelectorAll(".level-overview")].some((strip) => {
  const data = strip.getContext("2d").getImageData(0, 0, strip.width, strip.height).data;
  for (let k = 0; k < data.length; k += 4) if (data[k] === 255 && data[k + 1] === 217 && data[k + 2] === 106) return true;
  return false;
});
const url = `${(process.env.DEMO_URL || "http://127.0.0.1:4173").replace(/\/$/, "")}/?level=0&pace=0`;
const errors = [];
await mkdir("test-results", { recursive: true });
try {
  for (const [name, options] of [
    ["desktop", { viewport: { width: 1440, height: 1100 } }],
    ["phone", { viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true }],
  ]) {
    const page = await browser.newPage(options);
    page.on("pageerror", (e) => errors.push(e.message));
    await page.addInitScript(({ key, catalog }) => {
      localStorage.setItem(key, "seen");
      const maps = new Map(catalog.maps.map(m => [m.id, m]));
      const RealWorker = window.Worker;
      window.tdTestHeat = new Map();
      const cellKey = p => `${p.level}:${Math.floor((p.x + 8) / 32)}:${Math.floor(p.y / 32)}`;
      window.Worker = class extends RealWorker {
        constructor(...args) {
          super(...args);
          this.addEventListener("message", ({ data }) => {
            if (!["ready", "batch"].includes(data.type)) return;
            if (!window.tdTestHeat.has(data.active)) window.tdTestHeat.set(data.active, { motion: new Set(), endpoints: new Set() });
            const cells = window.tdTestHeat.get(data.active);
            for (const point of data.type === "ready" ? [data.state] : data.points) {
              const o = point.observation;
              if (maps.has(o.level) && !o.loading && o.health > 0)
                cells.endpoints.add(cellKey(o));
            }
            for (const trail of data.motion || []) for (let i = 0; i < trail.length; i += 5) {
              const map = maps.get(trail[i + 1]);
              if (!map || trail[i + 2] >= map.width) continue;
              const p = { level: trail[i + 1], x: trail[i + 2], y: trail[i + 3] };
              if (p.x + 8 >= map.width || p.y + 8 < 0 || p.y + 8 >= map.height) continue;
              cells.motion.add(cellKey(p));
            }
          });
        }
      };
    }, { key: TOUR_KEY, catalog });
    await page.goto(url);
    await page.waitForSelector("#attempts", { state: "attached" });
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "movement");
    assert.deepEqual(await page.locator("#visualization button").allTextContents(), ["Movement", "Heatmap"]);
    await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 1000);
    await setView(page,'movement');
    await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].some((c) => Number(c.dataset.swarmCount) > 0));
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "movement");
    assert.equal(await page.locator("#heat-legend").count(), 0);
    const target = page.locator('.map-row .area-map');
    assert.equal(await target.getAttribute("data-overlay"), "movement");
    assert.equal(await target.evaluate((c) => getComputedStyle(c).filter), "none", "Movement must retain the original map colors");
    assert.equal(await target.getAttribute("data-trace-points"), "0");
    assert.equal(await target.getAttribute("data-marker-frame"), "");
    await page.locator("#pause").click();
    await page.waitForTimeout(150);
    const attempts = await page.locator("#attempts").textContent();
    const coverage = await page.evaluate(() => {
      const cells = window.tdTestHeat.get(0);
      return { expected: new Set([...cells.motion, ...cells.endpoints]).size, transit: [...cells.motion].filter(k => !cells.endpoints.has(k)).length, actual: Number(document.querySelector("#cells").textContent.replaceAll(",", "")) };
    });
    assert.ok(coverage.transit > 0, "The real search must cross cells between reported endpoints");
    assert.equal(coverage.actual, coverage.expected, "The UI must paint the actual sampled footprint, including transit cells");
    const pixels = () => target.evaluate((c) => c.toDataURL());
    const before = await pixels();
    await page.waitForTimeout(220);
    assert.equal(await pixels(), before, "Paused search must freeze the actual incoming attempts");
    assert.equal(await page.locator("#attempts").textContent(), attempts);
    await page.screenshot({ path: `test-results/swarm-${name}.png` });
    await setView(page,'heat');
    assert.equal(await target.getAttribute("data-overlay"), "heat");
    assert.equal(await target.getAttribute("data-swarm-count"), "0");
    assert.equal(await page.locator("#heat-legend").count(), 0);
    await page.waitForTimeout(1700);
    const heatPixels = await pixels();
    await setView(page,'both');
    assert.equal(await target.getAttribute("data-overlay"), "both");
    assert.ok(Number(await target.getAttribute("data-swarm-count")) > 0);
    assert.equal(await page.locator("#heat-legend").count(), 0);
    assert.equal(await target.evaluate((c) => getComputedStyle(c).filter), "none", "Both must preserve the heat colors");
    const bothPixels = await pixels();
    const preserved = await page.evaluate(async ([before, after]) => {
      const decode = async (url) => {
        const image = new Image(); image.src = url; await image.decode();
        const canvas = document.createElement('canvas');
        canvas.width = image.naturalWidth; canvas.height = image.naturalHeight;
        const ctx = canvas.getContext('2d'); ctx.drawImage(image, 0, 0);
        return ctx.getImageData(96, 0, canvas.width - 96, canvas.height).data;
      };
      const a = await decode(before), b = await decode(after);
      let equal = 0;
      for (let i = 0; i < a.length; i += 4)
        if (a[i] === b[i] && a[i + 1] === b[i + 1] && a[i + 2] === b[i + 2]) equal++;
      return equal / (a.length / 4);
    }, [heatPixels, bothPixels]);
    assert.ok(preserved > 0.6 && preserved < 1, `Both must retain the heatmap and add actual sprite pixels (${preserved})`);
    await page.waitForTimeout(220);
    assert.equal(await pixels(), bothPixels, "Both must freeze heat and incoming attempts together");
    await page.locator("#pause").click();
    await page.waitForTimeout(220);
    assert.notEqual(await pixels(), bothPixels, "New attempts must animate while search runs");
    await page.locator("#pause").click();
    await page.waitForTimeout(150);
    await page.screenshot({ path: `test-results/swarm-both-${name}.png` });
    await page.locator("#tour-open").click();
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "movement", "The tour opens on real Movement");
    if (name === "desktop") {
      for (const step of [1]) {
        await page.locator("#tour-next").click();
        await page.waitForFunction(step => +document.querySelector("#guided-tour").dataset.step === step && document.querySelector(".tour-card").getAttribute("aria-busy") === "false", step);
      }
    }
    await page.locator("#tour-skip").click();
    if (name === "desktop") {
      if (await page.locator("#pause").getAttribute("aria-label") === "Pause Search") await page.locator("#pause").click();
      await setView(page,'both');
      const room = await (await page.waitForFunction(() => [...document.querySelectorAll('.area-map')].find((c) => c.offsetParent && Number(c.dataset.tracePoints) > 0)?.dataset.map)).jsonValue();
      const routeMap = page.locator(`.map-row[data-map="${room}"] .area-map`);
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#verification').textContent === 'Exact replay ✓' && [...document.querySelectorAll('.area-map')].some((c) => Number(c.dataset.tracePoints) > 0));
      assert.equal(await page.locator("#visualization").getAttribute("data-value"), 'both', 'Inspecting a populated cell must keep Both');
      assert.equal(await page.locator('#inspector').isVisible(), true);
      await page.screenshot({ path: 'test-results/swarm-both-route.png' });
      await setView(page,'movement');
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#visualization').dataset.value === 'movement' && document.querySelector('#verification').textContent === 'Exact replay ✓' && [...document.querySelectorAll('.area-map')].some(c => Number(c.dataset.tracePoints) > 0));
      await setView(page,'both');
      await page.locator("#take-control").click();
      await page.keyboard.down("ArrowRight");
      await page.waitForTimeout(140);
      await page.keyboard.up("ArrowRight");
      assert.equal(await page.locator("#take-control").isVisible(), false);
      await page.locator("#search-here").click();
      await page.waitForFunction(() => document.querySelector("#branch-tree button[aria-pressed=true]").dataset.search === "1" && !document.querySelector("#branch-tree button[aria-pressed=true]").disabled && document.querySelector("#inspector").hidden);
      await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 30);
      await page.locator("#pause").click();
      await setView(page,'both');
      await page.waitForFunction(stripHasSprites);
      const ghosts = await page.locator(".area-map[data-panel=\"0\"]").evaluateAll((cs) => cs.reduce((n, c) => n + Number(c.dataset.swarmCount), 0));
      const branchAttempts = Number((await page.locator("#attempts").textContent()).replaceAll(",", ""));
      assert.ok(ghosts <= branchAttempts + 2, `A new branch must not display the original search's players (${ghosts} sprites, ${branchAttempts} attempts)`);
      await page.locator('#branch-tree button[data-search="0"]').click();
      await page.waitForFunction(() => document.querySelector("#pause").getAttribute("aria-label") === "Pause Search");
      await page.locator("#pause").click();
      await page.waitForFunction(stripHasSprites);
      assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both");
    } else await setView(page,'both');
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both");
    await page.locator("#reset").click();
    await page.waitForFunction(() => document.querySelector("#reset").disabled === false);
    await page.waitForFunction(() => Number(document.querySelector('.map-row .area-map').dataset.swarmCount) < 100);
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both", "Restart must preserve the chosen view");
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log("Visualization: Heatmap, Movement and Both, preserved heat colors, one-shot original sprites with shared pause/resume, retained route inspection, branch isolation, restart, guided-tour handoff and desktop/phone layouts passed.");
} finally { await browser.close(); }
