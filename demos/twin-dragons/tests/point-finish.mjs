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
  assert.deepEqual(errors, []);
  console.log("Point-only Famidash completions preserve the pending camera destination across multiple levels, without finishing trails.");
} finally { await browser.close(); }
