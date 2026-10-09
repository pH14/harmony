// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { readFile, mkdir } from "node:fs/promises";
import { createEngine } from "../src/emulator.js";
import { RolloutRecorder, motionPoint } from "../src/swarm.js";
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
for (const before of expected) {
  recorder.begin();
  const batch = JSON.parse(recorded.advance(2)), trails = recorder.finish();
  assert.deepEqual(batch, before.batch, "Sampling must preserve all random choices, work counts and retained states");
  for (const [id, snapshot] of before.snapshots)
    assert.deepEqual(recorded.snapshot(id), snapshot, "Sampling must preserve every archived endpoint byte");
  assert.equal(trails.length, 2);
  assert.ok(trails.every((trail) => trail.length <= 1205));
  recordedPoints += trails.reduce((n, t) => n + t.length / 5, 0);
}
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
    await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
    await page.goto(url);
    await page.waitForSelector("#attempts");
    assert.equal(await page.getByRole("combobox", { name: "Visualization" }).inputValue(), "heat");
    assert.deepEqual(await page.locator("#visualization option").allTextContents(), ["Heatmap", "Movement", "Both"]);
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
    await page.locator("#visualization").selectOption("movement");
    await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].some((c) => Number(c.dataset.swarmCount) > 100));
    assert.equal(await page.locator("#visualization").inputValue(), "movement");
    assert.equal(await page.locator("#heat-legend").isVisible(), false);
    const target = page.locator('.map-row[data-map="0"] canvas');
    assert.equal(await target.getAttribute("data-overlay"), "movement");
    assert.equal(await target.getAttribute("data-trace-points"), "0");
    assert.equal(await target.getAttribute("data-marker-frame"), "");
    await page.locator("#pause").click();
    await page.waitForTimeout(150);
    const attempts = await page.locator("#attempts").innerText();
    const pixels = () => target.evaluate((c) => c.toDataURL());
    const before = await pixels();
    await page.waitForTimeout(220);
    assert.notEqual(await pixels(), before, "Real recorded sprites must move while search is paused");
    assert.equal(await page.locator("#attempts").innerText(), attempts);
    await page.screenshot({ path: `test-results/swarm-${name}.png` });
    await page.locator("#visualization").selectOption("heat");
    assert.equal(await target.getAttribute("data-overlay"), "heat");
    assert.equal(await target.getAttribute("data-swarm-count"), "0");
    assert.equal(await page.locator("#heat-legend").isVisible(), true);
    await page.waitForTimeout(1700);
    const heatPixels = await pixels();
    await page.locator("#visualization").selectOption("both");
    assert.equal(await target.getAttribute("data-overlay"), "both");
    assert.ok(Number(await target.getAttribute("data-swarm-count")) > 100);
    assert.equal(await page.locator("#heat-legend").isVisible(), true);
    assert.equal(await target.evaluate((c) => getComputedStyle(c).filter), "none", "Both must preserve the heat colors");
    assert.equal(await page.locator('.map-card canvas').first().getAttribute('data-overlay'), 'both');
    const bothPixels = await pixels();
    const preserved = await page.evaluate(async ([before, after]) => {
      const decode = async (url) => {
        const image = new Image(); image.src = url; await image.decode();
        const canvas = document.createElement('canvas');
        canvas.width = image.naturalWidth; canvas.height = image.naturalHeight;
        const ctx = canvas.getContext('2d'); ctx.drawImage(image, 0, 0);
        return ctx.getImageData(0, 0, canvas.width, canvas.height).data;
      };
      const a = await decode(before), b = await decode(after);
      let equal = 0;
      for (let i = 0; i < a.length; i += 4)
        if (a[i] === b[i] && a[i + 1] === b[i + 1] && a[i + 2] === b[i + 2]) equal++;
      return equal / (a.length / 4);
    }, [heatPixels, bothPixels]);
    assert.ok(preserved > 0.8 && preserved < 1, `Both must retain the heatmap and add actual sprite pixels (${preserved})`);
    await page.waitForTimeout(220);
    assert.notEqual(await pixels(), bothPixels, "Movement must animate above frozen heat in Both");
    assert.equal(await page.locator("#attempts").innerText(), attempts);
    await page.screenshot({ path: `test-results/swarm-both-${name}.png` });
    await page.locator("#tour-open").click();
    assert.equal(await page.locator("#visualization").inputValue(), "heat");
    if (name === "desktop") {
      await page.locator("#tour-next").click();
      await page.waitForFunction(() => document.querySelector("#guided-tour").dataset.step === "1" && document.querySelector(".tour-card").getAttribute("aria-busy") === "false");
    }
    await page.locator("#tour-skip").click();
    if (name === "desktop") {
      await page.locator("#visualization").selectOption("both");
      const traced = page.locator('.area-map').filter({ visible: true });
      const room = await traced.evaluateAll((cs) => cs.find((c) => Number(c.dataset.tracePoints) > 0)?.dataset.map);
      assert.notEqual(room, undefined);
      const routeMap = page.locator(`.map-row[data-map="${room}"] canvas`);
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#verification').textContent === 'Exact replay ✓' && [...document.querySelectorAll('.area-map')].some((c) => Number(c.dataset.tracePoints) > 0));
      assert.equal(await page.locator('#visualization').inputValue(), 'both', 'Inspecting a populated cell must keep Both');
      assert.equal(await page.locator('#inspector').isVisible(), true);
      await page.screenshot({ path: 'test-results/swarm-both-route.png' });
      await page.locator('#visualization').selectOption('movement');
      await routeMap.focus();
      await page.keyboard.press('Enter');
      await page.waitForFunction(() => document.querySelector('#visualization').value === 'heat' && document.querySelector('#verification').textContent === 'Exact replay ✓');
      await page.locator('#visualization').selectOption('both');
      await page.locator("#take-control").click();
      await page.keyboard.down("ArrowRight");
      await page.waitForTimeout(140);
      await page.keyboard.up("ArrowRight");
      await page.locator("#take-control").click();
      await page.locator("#search-here").click();
      await page.waitForFunction(() => document.querySelector("#branch-choice").value === "1" && !document.querySelector("#branch-choice").disabled && document.querySelector("#inspector").hidden);
      await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 30);
      await page.locator("#pause").click();
      await page.locator("#visualization").selectOption("both");
      await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].some((c) => Number(c.dataset.swarmCount) > 0));
      const branchAttempts = Number((await page.locator("#attempts").innerText()).replaceAll(",", ""));
      const ghosts = () => page.locator(".area-map").evaluateAll((cs) => cs.reduce((n, c) => n + Number(c.dataset.swarmCount), 0));
      assert.ok(await ghosts() <= branchAttempts + 2, "A new branch must not display the original search's Novas");
      await page.locator("#branch-choice").selectOption("0");
      await page.waitForFunction(() => document.querySelector("#pause").getAttribute("aria-label") === "Pause Search");
      await page.locator("#pause").click();
      await page.waitForFunction(() => [...document.querySelectorAll(".area-map")].reduce((n, c) => n + Number(c.dataset.swarmCount), 0) > 100);
      assert.equal(await page.locator("#visualization").inputValue(), "both");
    } else await page.locator("#visualization").selectOption("both");
    assert.equal(await page.locator("#visualization").inputValue(), "both");
    await page.locator("#reset").click();
    await page.waitForFunction(() => document.querySelector("#reset").disabled === false);
    await page.waitForFunction(() => Number(document.querySelector('.map-row[data-map="0"] canvas').dataset.swarmCount) < 100);
    assert.equal(await page.locator("#visualization").inputValue(), "both", "Restart must preserve the chosen view");
    await page.close();
  }
  assert.deepEqual(errors, []);
  console.log("Visualization: Heatmap, Movement and Both, preserved heat colors, animated neutral sprites, retained route inspection, branch isolation, restart, guided-tour handoff and desktop/phone layouts passed.");
} finally { await browser.close(); }
