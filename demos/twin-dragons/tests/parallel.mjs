// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const errors = [];
async function archive(workers) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  page.on("pageerror", (e) => errors.push(e.message));
  await page.addInitScript((key) => {
    localStorage.setItem(key, "seen");
    const RealWorker = window.Worker;
    window.tdStates = new Map();
    window.Worker = class extends RealWorker {
      constructor(...args) {
        super(...args);
        window.testWorker = this;
        this.addEventListener("message", ({ data }) => {
          if (data.type === "states" && data.request >= 9000) window.tdStates.set(data.request, data.states);
        });
      }
    };
  }, TOUR_KEY);
  await page.goto(`${url}/?workers=${workers}&pace=0`);
  await page.waitForFunction(() => Number(document.querySelector("#attempts").textContent.replaceAll(",", "")) >= 1500, null, { timeout: 120000 });
  await page.locator("#pause").click();
  assert.equal(await page.locator("#workers").textContent(), String(workers));
  const states = [];
  for (let request = 9000; request < 9010; request++) {
    const ids = Array.from({ length: 12 }, (_, i) => (request - 9000) * 12 + i + 1);
    await page.evaluate(([ids, request]) => window.testWorker.postMessage({ type: "states", ids, request }), [ids, request]);
    await page.waitForFunction((request) => window.tdStates.has(request), request);
    states.push(...(await page.evaluate((request) => window.tdStates.get(request), request)).map((s) => ({ id: s.id, actions: s.actions, digest: s.digest })));
  }
  await page.close();
  return states;
}
try {
  const first = await archive(3), second = await archive(3), single = await archive(1);
  assert.equal(first.length, 120, "The first 120 archive entries are retained histories");
  assert.deepEqual(second, first, "A fixed seed and worker count must archive the same histories on every page load");
  assert.deepEqual(single, first, "One rollout worker must archive the same histories as three");
  assert.deepEqual(errors, []);
  console.log("Parallel search: three workers archive identical histories and snapshot digests across page loads and match one worker.");
} finally {
  await browser.close();
}
