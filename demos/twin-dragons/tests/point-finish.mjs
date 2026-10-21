// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  const errors = [];
  page.on("pageerror", error => errors.push(error.message));
  await page.addInitScript(key => {
    localStorage.setItem(key, "seen");
    Object.defineProperty(navigator, "hardwareConcurrency", { get: () => 3 });
    const drawImage = CanvasRenderingContext2D.prototype.drawImage;
    CanvasRenderingContext2D.prototype.drawImage = function (...args) {
      const result = drawImage.apply(this, args);
      if (window.terminalMotion && this.canvas.classList.contains("area-map") &&
          args.length === 9 && args[0] instanceof HTMLImageElement && args[1] > 0 &&
          window.terminalMotion.length < 5000)
        window.terminalMotion.push({ at: performance.now(), x: args[5] });
      return result;
    };
    const Worker = window.Worker;
    window.Worker = class extends Worker {
      constructor(url, options) {
        super(url, options);
        if (!String(url).includes("search-worker")) return;
        let handler;
        Object.defineProperty(this, "onmessage", { get: () => handler, set: value => { handler = value; } });
        this.addEventListener("message", event => {
          if (event.data.type === "ready") {
            window.pointFinishRoot = event.data.state.observation;
            handler?.(event);
          } else if (event.data.type === "batch") window.pointFinishBatch = event.data;
          else handler?.(event);
        });
        window.pointFinishDeliver = data => handler({ data });
      }
    };
  }, TOUR_KEY);
  await page.goto(`${process.env.DEMO_URL || "http://127.0.0.1:4173"}/?game=famidash&level=0&workers=1&pace=0`);
  await page.waitForFunction(() => window.pointFinishBatch && document.querySelector(".area-map")?.dataset.map === "0", null, { timeout: 60000 });
  // Both completions survive transport sampling; their finishing motion trails do not.
  await page.evaluate(() => {
    const observation = window.pointFinishRoot;
    const points = [
      { observation: { ...observation, level: 0, cleared: 1 }, retained: null, frame: 0 },
      { observation: { ...observation, level: 1, cleared: 2 }, retained: null, frame: 0 },
      { observation: { ...observation, level: 2, cleared: 2 }, retained: null, frame: 0 },
    ];
    pointFinishDeliver({ ...pointFinishBatch, points, motion: [], finishes: [], exits: [] });
  });
  await page.waitForTimeout(100);
  assert.equal(await page.locator(".area-map").getAttribute("data-map"), "0", "Point completion waits for sprites in the destination");
  await page.evaluate(() => {
    const motion = Array.from({ length: 3 }, (_, i) => Uint16Array.from([0, 2, 120 + i * 20, 120, 0, 30, 2, 140 + i * 20, 120, 0, 120, 2, 160 + i * 20, 120, 0]));
    pointFinishDeliver({ ...pointFinishBatch, points: [], motion, finishes: [], exits: [] });
  });
  await page.waitForFunction(() => document.querySelector(".area-map")?.dataset.map === "2", null, { timeout: 3000 });
  const finalBatch = async (level) => page.evaluate(level => {
    window.terminalMotion = [];
    const canvas = document.querySelector(".area-map");
    const x = level === 0 ? Math.round(Number(canvas.dataset.centerX)) : 120;
    const y = level === 0 ? Math.round(Number(canvas.dataset.centerY)) : 120;
    window.terminalFixture = { x, y, map: canvas.dataset.map };
    const motion = Array.from({ length: 4 }, (_, i) => Uint16Array.from([
      0, level, x + i * 10, y, 2, 60, level, x + 60 + i * 10, y, 2,
    ]));
    pointFinishDeliver({ ...pointFinishBatch, executions: 123, frames: 4560,
      points: [], motion, finishes: [], exits: [], stopped: true });
    pointFinishDeliver({ type: "limit", won: false, finished: true });
  }, level);
  await finalBatch(2);
  await page.waitForFunction(() => terminalMotion.length > 8, null, { timeout: 2000 });
  await page.waitForTimeout(650);
  const moving = await page.evaluate(() => terminalMotion);
  const firstAt = moving[0].at, lastAt = moving.at(-1).at;
  const early = moving.filter(p => p.at < firstAt + 60).map(p => p.x);
  const late = moving.filter(p => p.at > lastAt - 60).map(p => p.x);
  assert.ok(Math.max(...late) > Math.max(...early) + 20, "Final trails still advance at their recorded speed after limit");
  assert.equal(await page.locator("#status").textContent(), "Search finished");
  assert.equal(await page.locator("#pause").isDisabled(), true);
  await page.waitForTimeout(1800);
  const settled = await page.evaluate(() => ({ count: terminalMotion.length,
    camera: document.querySelector(".area-map").dataset.centerX,
    executions: document.querySelector("#hero-timelines").textContent,
    frames: document.querySelector("#work").textContent }));
  await page.waitForTimeout(250);
  assert.deepEqual(await page.evaluate(() => ({ count: terminalMotion.length,
    camera: document.querySelector(".area-map").dataset.centerX,
    executions: document.querySelector("#hero-timelines").textContent,
    frames: document.querySelector("#work").textContent })), settled, "Presentation and counters freeze after the retained tail expires");
  assert.equal(Number(settled.executions.replace(/\D/g, "")), 123);
  assert.equal(Number(settled.frames.replace(/\D/g, "")), 4560);
  assert.equal(await page.locator("#pause").getAttribute("aria-label"), "Resume Search");
  assert.equal(await page.locator("#status").textContent(), "Search finished");
  assert.equal(await page.locator("#goal-status").textContent(), "Search finished");
  for (const mode of ["paused", "reduced-motion"]) {
    await page.emulateMedia({ reducedMotion: mode === "reduced-motion" ? "reduce" : "no-preference" });
    await page.reload();
    await page.waitForFunction(() => window.pointFinishBatch && document.querySelector(".area-map")?.dataset.map === "0", null, { timeout: 15000 });
    if (mode === "paused") await page.locator("#pause").click();
    await finalBatch(0);
    await page.waitForTimeout(150);
    const first = await page.evaluate(() => terminalMotion.map(p => p.x));
    await page.waitForTimeout(1200);
    const beyondTTL = await page.evaluate(() => terminalMotion.length);
    await page.waitForTimeout(200);
    const final = await page.evaluate(() => terminalMotion.map(p => p.x));
    assert.ok(first.length > 0 && beyondTTL > first.length && final.length > beyondTTL, mode + " paints frozen positions beyond the actual one-second trail TTL: " + JSON.stringify({ first, final, fixture: await page.evaluate(() => terminalFixture), canvas: await page.locator(".area-map").evaluate(c => ({...c.dataset})) }));
    assert.deepEqual([...new Set(final)].sort(), [...new Set(first)].sort(), mode + " never advances final motion");
    assert.equal(await page.locator("#pause").isDisabled(), true);
  }
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await page.reload();
  await page.waitForFunction(() => window.pointFinishBatch && document.querySelector(".area-map")?.dataset.map === "0", null, { timeout: 15000 });
  await finalBatch(0);
  await page.waitForTimeout(200);
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.evaluate(() => { window.terminalMotion = []; });
  await page.waitForTimeout(1200);
  const reduced = await page.evaluate(() => terminalMotion.map(p => p.x));
  await page.waitForTimeout(200);
  const later = await page.evaluate(() => terminalMotion.map(p => p.x));
  assert.ok(reduced.length > 0 && later.length > reduced.length, "Enabling reduced motion mid-tail freezes rather than expiring its trails");
  assert.deepEqual([...new Set(later)].sort(), [...new Set(reduced)].sort());
  assert.equal(await page.locator("#pause").getAttribute("aria-label"), "Resume Search");
  assert.equal(await page.locator("#status").textContent(), "Search finished");
  assert.deepEqual(errors, []);
  console.log("Point-only level transitions and terminal trail drain, final freeze/counters, manual pause and reduced motion passed.");
} finally { await browser.close(); }
