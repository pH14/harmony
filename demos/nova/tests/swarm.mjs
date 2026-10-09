// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { readFile, mkdir } from "node:fs/promises";
import { createEngine } from "../src/emulator.js";
import { RolloutRecorder, motionPoint } from "../src/swarm.js";
import { Heatmap, cellKey } from "../src/heat.js";
import { project, isMapEvidence } from "../src/world.js";
import { TOUR_KEY } from "../src/tour.js";
import init, { Explorer } from "../rust/pkg/nova_browser.js";
const base = new URL("../public/", import.meta.url);
const engine = await createEngine(base, {
  rom: await readFile(new URL("nova.nes", base)),
  wasmBinary: await readFile(new URL("engine/quicknes.wasm", base)),
});
await init({ module_or_path: await readFile(new URL("../rust/pkg/nova_browser_bg.wasm", import.meta.url)) });
globalThis.harmonyEngine = engine;
const root = engine.boot(), catalog = JSON.parse(await readFile(new URL("maps.json", base)));
const owners = new Map(catalog.levels.flatMap((level) => level.rooms.map((room) => [room, level.id])));
const original = new Explorer(2), expected = [];
for (let i = 0; i < 64; i++) {
  const batch = JSON.parse(original.advance(2));
  expected.push({ batch, snapshots: batch.points.filter((p) => p.retained !== null).map((p) => [p.retained, original.snapshot(p.retained)]) });
}
original.free();
engine.restore(root);
const recorder = new RolloutRecorder(engine, () => motionPoint(engine, owners)), recorded = new Explorer(2);
let recordedPoints = 0;
const sampledHeat = new Heatmap(), endpointKeys = new Set(), mapCatalog = new Map(catalog.maps.map(m => [m.id, m]));
for (const before of expected) {
  recorder.begin();
  const batch = JSON.parse(recorded.advance(2)), trails = recorder.finish();
  assert.deepEqual(batch, before.batch, "Sampling must preserve all random choices, work counts and retained states");
  for (const [id, snapshot] of before.snapshots)
    assert.deepEqual(recorded.snapshot(id), snapshot, "Sampling must preserve every archived endpoint byte");
  assert.equal(trails.length, 2);
  assert.ok(trails.every((trail) => trail.length <= 1205));
  recordedPoints += trails.reduce((n, t) => n + t.length / 5, 0);
  sampledHeat.visitMotion(trails, mapCatalog, 0);
  for (const point of batch.points) {
    const o = point.observation, map = mapCatalog.get(o.level);
    if (!map || !isMapEvidence(o, catalog.levels)) continue;
    const observation = project(o, map);
    endpointKeys.add(cellKey(observation));
    sampledHeat.retain({ ...point, observation }, 0);
  }
}
const transitCells = [...sampledHeat.cells.values()].filter(c => !endpointKeys.has(c.key));
assert.ok(transitCells.length > 0, "Real attempts must mark crossed cells missing from action endpoints");
assert.ok(transitCells.every(c => c.heat > 0 && c.ids.length === 0), "Transit cells must not invent retained histories");
console.log(`Sampled heat: ${transitCells.length} real transit cells missing from endpoint-only heat, without fabricated states.`);
recorded.free();
for (const name of ["main-exit", "level-two-2"]) {
  const tape = JSON.parse(await readFile(new URL(`fixtures/${name}.json`, import.meta.url)));
  engine.restore(root);
  for (const a of tape.actions) engine.run(a.buttons, a.frames);
  const endpoint = engine.capture();
  recorder.begin();
  engine.restore(root);
  for (const a of tape.actions) engine.run(a.buttons, a.frames);
  const [trail] = recorder.finish();
  assert.deepEqual(engine.capture(), endpoint, "Sampled door and campaign transitions must match unsampled execution");
  const rooms = new Set([...trail].filter((_, index) => index % 5 === 1));
  assert.ok(rooms.has(49) && rooms.has(45));
}
console.log(`Swarm sampling: 128 actual rollouts, ${recordedPoints} poses, every archived endpoint and recorded campaign transitions match unsampled execution.`);

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
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
      const owners = new Map(catalog.levels.flatMap(l => l.rooms.map(r => [r, l.id])));
      const RealWorker = window.Worker;
      window.novaTestHeat = new Map();
      const cellKey = p => `${p.level}:${Math.floor((p.x % maps.get(p.level).width) / 32)}:${Math.floor((Math.floor(p.x / maps.get(p.level).width) * 224 + p.y) / 32)}`;
      window.Worker = class extends RealWorker {
        constructor(...args) {
          super(...args);
          this.addEventListener("message", ({ data }) => {
            if (!["ready", "batch"].includes(data.type)) return;
            if (!window.novaTestHeat.has(data.active)) window.novaTestHeat.set(data.active, { motion: new Set(), endpoints: new Set() });
            const cells = window.novaTestHeat.get(data.active);
            for (const point of data.type === "ready" ? [data.state] : data.points) {
              const o = point.observation;
              if (maps.has(o.level) && !o.reload && o.program_bank === 9 && owners.get(o.level) === o.selected_level && owners.get(o.checkpoint_level) === o.selected_level)
                cells.endpoints.add(cellKey(o));
            }
            for (const trail of data.motion || []) for (let i = 0; i < trail.length; i += 5) {
              const map = maps.get(trail[i + 1]);
              if (!map || trail[i + 2] >= map.runtimeWidth) continue;
              const p = { level: trail[i + 1], x: trail[i + 2], y: trail[i + 3] };
              const y = Math.floor(p.x / map.width) * 224 + p.y;
              if (y < 0 || y >= map.height) continue;
              cells.motion.add(cellKey(p));
            }
          });
        }
      };
    }, { key: TOUR_KEY, catalog });
    await page.goto(url);
    await page.waitForSelector("#attempts");
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "movement");
    assert.deepEqual(await page.locator("#visualization button").allTextContents(), ["Movement", "Heatmap", "Both"]);
    const bakedNovaPixels = await page.evaluate(async () => {
      const image = new Image();
      image.src = new URL("maps/0.png", location.href);
      await image.decode();
      const canvas = document.createElement("canvas");
      canvas.width = image.naturalWidth;
      canvas.height = image.naturalHeight;
      const context = canvas.getContext("2d");
      context.drawImage(image, 0, 0);
      const pixels = context.getImageData(40, 172, 24, 28).data;
      let blue = 0;
      for (let i = 0; i < pixels.length; i += 4)
        if (pixels[i] === 65 && pixels[i + 1] === 64 && pixels[i + 2] === 255) blue++;
      return blue;
    });
    assert.equal(bakedNovaPixels, 0, "The map artwork must not contain a stationary Nova at the starting position");
    await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 240);
    await page.locator('[data-viz="movement"]').click();
    await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].some((c) => Number(c.dataset.swarmCount) > 0));
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "movement");
    assert.equal(await page.locator("#heat-legend").count(), 0);
    const target = page.locator('.map-row[data-map="0"] canvas');
    assert.equal(await target.getAttribute("data-overlay"), "movement");
    assert.equal(await target.evaluate((c) => getComputedStyle(c).filter), "none", "Movement must retain the original map colors");
    assert.equal(await target.getAttribute("data-trace-points"), "0");
    assert.equal(await target.getAttribute("data-marker-frame"), "");
    await page.locator("#pause").click();
    await page.waitForTimeout(150);
    const attempts = await page.locator("#attempts").innerText();
    const coverage = await page.evaluate(() => {
      const cells = window.novaTestHeat.get(0);
      return { expected: new Set([...cells.motion, ...cells.endpoints]).size, transit: [...cells.motion].filter(k => !cells.endpoints.has(k)).length, actual: Number(document.querySelector("#cells").textContent.replaceAll(",", "")) };
    });
    assert.ok(coverage.transit > 0, "The real search must cross cells between reported endpoints");
    assert.equal(coverage.actual, coverage.expected, "The UI must paint the actual sampled footprint, including transit cells");
    const pixels = () => target.evaluate((c) => c.toDataURL());
    const before = await pixels();
    await page.waitForTimeout(220);
    assert.equal(await pixels(), before, "Paused search must freeze the actual incoming attempts");
    assert.equal(await page.locator("#attempts").innerText(), attempts);
    await page.screenshot({ path: `test-results/swarm-${name}.png` });
    await page.locator('[data-viz="heat"]').click();
    assert.equal(await target.getAttribute("data-overlay"), "heat");
    assert.equal(await target.getAttribute("data-swarm-count"), "0");
    assert.equal(await page.locator("#heat-legend").count(), 0);
    await page.waitForTimeout(1700);
    const heatPixels = await pixels();
    await page.locator('[data-viz="both"]').click();
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
    assert.ok(preserved > 0.8 && preserved < 1, `Both must retain the heatmap and add actual sprite pixels (${preserved})`);
    await page.waitForTimeout(220);
    assert.equal(await pixels(), bothPixels, "Both must freeze heat and incoming attempts together");
    await page.locator("#pause").click();
    await page.waitForTimeout(220);
    assert.notEqual(await pixels(), bothPixels, "New attempts must animate while search runs");
    await page.locator("#pause").click();
    await page.waitForTimeout(150);
    await page.screenshot({ path: `test-results/swarm-both-${name}.png` });
    await page.locator("#tour-open").click();
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "heat");
    if (name === "desktop") {
      await page.locator("#tour-next").click();
      await page.waitForFunction(() => document.querySelector("#guided-tour").dataset.step === "1" && document.querySelector(".tour-card").getAttribute("aria-busy") === "false");
    }
    await page.locator("#tour-skip").click();
    if (name === "desktop") {
      await page.locator('[data-viz="both"]').click();
      const traced = page.locator('.area-map').filter({ visible: true });
      const room = await traced.evaluateAll((cs) => cs.find((c) => Number(c.dataset.tracePoints) > 0)?.dataset.map);
      assert.notEqual(room, undefined);
      const routeMap = page.locator(`.map-row[data-map="${room}"] canvas`);
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#verification').textContent === 'Exact replay ✓' && [...document.querySelectorAll('.area-map')].some((c) => Number(c.dataset.tracePoints) > 0));
      assert.equal(await page.locator("#visualization").getAttribute("data-value"), 'both', 'Inspecting a populated cell must keep Both');
      assert.equal(await page.locator('#inspector').isVisible(), true);
      await page.screenshot({ path: 'test-results/swarm-both-route.png' });
      await page.locator('[data-viz="movement"]').click();
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#visualization').dataset.value === 'movement' && document.querySelector('#verification').textContent === 'Exact replay ✓' && [...document.querySelectorAll('.area-map')].some(c => Number(c.dataset.tracePoints) > 0));
      await page.locator('[data-viz="both"]').click();
      await page.locator("#take-control").click();
      await page.keyboard.down("ArrowRight");
      await page.waitForTimeout(140);
      await page.keyboard.up("ArrowRight");
      assert.equal(await page.locator("#take-control").isVisible(), false);
      await page.locator("#search-here").click();
      await page.waitForFunction(() => document.querySelector("#branch-tree button[aria-pressed=true]").dataset.search === "1" && !document.querySelector("#branch-tree button[aria-pressed=true]").disabled && document.querySelector("#inspector").hidden);
      await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 30);
      await page.locator("#pause").click();
      await page.locator('[data-viz="both"]').click();
      await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].some((c) => Number(c.dataset.swarmCount) > 0));
      const branchAttempts = Number((await page.locator("#attempts").innerText()).replaceAll(",", ""));
      const ghosts = () => page.locator(".area-map").evaluateAll((cs) => cs.reduce((n, c) => n + Number(c.dataset.swarmCount), 0));
      assert.ok(await ghosts() <= branchAttempts + 2, "A new branch must not display the original search's Novas");
      await page.locator('#branch-tree button[data-search="0"]').click();
      await page.waitForFunction(() => document.querySelector("#pause").getAttribute("aria-label") === "Pause Search");
      await page.locator("#pause").click();
      await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].reduce((n, c) => n + Number(c.dataset.swarmCount), 0) > 0);
      assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both");
    } else await page.locator('[data-viz="both"]').click();
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both");
    await page.locator("#reset").click();
    await page.waitForFunction(() => document.querySelector("#reset").disabled === false);
    await page.waitForFunction(() => Number(document.querySelector('.map-row[data-map="0"] canvas').dataset.swarmCount) < 100);
    assert.equal(await page.locator("#visualization").getAttribute("data-value"), "both", "Restart must preserve the chosen view");
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log("Visualization: Heatmap, Movement and Both, preserved heat colors, one-shot original sprites with shared pause/resume, retained route inspection, branch isolation, restart, guided-tour handoff and desktop/phone layouts passed.");
} finally { await browser.close(); }
