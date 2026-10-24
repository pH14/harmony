// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });

async function open(query, controlled = false) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }), errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(({ key, controlled }) => {
    localStorage.setItem(key, "seen");
    window.clearingBatches = [];
    const Base = window.Worker;
    window.Worker = class extends Base {
      constructor(...args) {
        super(...args);
        if (controlled && String(args[0]).includes("search-worker")) {
          let handler;
          Object.defineProperty(this, "onmessage", { get: () => handler, set: value => { handler = value; } });
          this.addEventListener("message", event => {
            if (event.data.type === "batch") {
              window.overviewBatch = event.data;
              Base.prototype.postMessage.call(this, { type: "pause" });
            } else if (event.data.type !== "paused") handler?.(event);
          });
          window.overviewDeliver = data => handler({ data });
        }
        this.addEventListener("message", ({ data }) => {
          if (data?.type !== "batch") return;
          if ((data.finishes || []).some((index) => data.motion[index][1] === 0)) window.clearingBatches.push(performance.now());
        });
      }
    };
  }, { key: TOUR_KEY, controlled });
  await page.goto(`${url}/${query}`);
  return { page, errors };
}

async function widenFollowing(page, vertical) {
  await page.waitForFunction(() => window.overviewBatch && document.querySelector(".area-map"), null, { timeout: 15000 });
  await page.evaluate(vertical => {
    const canvas = document.querySelector(".area-map"), level = Number(canvas.dataset.map);
    const x = vertical ? Number(canvas.dataset.mapWidth) / 2 : Number(canvas.dataset.mapWidth) - 80;
    const y = vertical ? 100 : Number(canvas.dataset.mapHeight) / 2;
    const motion = Array.from({ length: 4 }, () => Uint16Array.from([0, level, x, y, 2, 600, level, x, y, 2]));
    overviewDeliver({ ...overviewBatch, points: [], motion, finishes: [], exits: [], stopped: false });
  }, vertical);
  await page.waitForFunction(() => Number(document.querySelector(".area-map").dataset.zoom) > 0, null, { timeout: 4000 });
  await page.waitForTimeout(1500);
}

for (const query of ["?level=0&pace=300", "?game=tobu-tobu-girl&level=0&pace=0"]) {
  const { page, errors } = await open(query);
  await page.waitForFunction(() => document.querySelector('.map-row[data-current="true"]')?.dataset.map === "1", null, { timeout: 120000 });
  const switched = await page.evaluate(() => performance.now());
  const batches = await page.evaluate(() => window.clearingBatches);
  assert.ok(batches.length, `${query}: the worker marks the rollouts that clear Level 1`);
  const first = Math.min(...batches);
  assert.ok(switched - first < 15000, `${query}: the map moves to Level 2 soon after the first batch that clears Level 1 (${Math.round(switched - first)} ms)`);
  await page.waitForFunction(() => Number(document.querySelector('.map-row[data-current="true"] .area-map')?.dataset.swarmCount) > 0, null, { timeout: 2000 });
  assert.deepEqual(errors, []);
  await page.close();
}
console.log("The map moves to the next level once the search has sprites there after clearing the shown one.");

const { page, errors } = await open("?game=tobu-tobu-girl&level=0&pace=0", true);
const strip = page.locator('.level-overview[data-map="0"]');
await strip.waitFor();
await widenFollowing(page, true);
const box = await strip.boundingBox();
await page.mouse.click(box.x + box.width / 2, box.y + box.height * 0.1);
const centerY = Number(await page.locator('.area-map[data-map="0"]').getAttribute("data-center-y"));
assert.ok(Math.abs(centerY - 200) < 20, `Clicking 10% down the level strip centers the view there (${centerY})`);
const view = await page.locator('.area-map[data-map="0"]').boundingBox();
assert.ok(box.x >= view.x + view.width && box.x - (view.x + view.width) < 40, "The level strip sits beside the game view");
assert.deepEqual(errors, []);
console.log("Tobu Tobu Girl shows a whole-level strip that moves the view.");

const { page: wide, errors: wideErrors } = await open("?level=0&pace=300", true);
const wideStrip = wide.locator('.level-overview[data-map="0"]');
await wideStrip.waitFor();
await widenFollowing(wide, false);
const wideBox = await wideStrip.boundingBox(), wideView = await wide.locator('.area-map[data-map="0"]').boundingBox();
assert.ok(wideBox.y >= wideView.y + wideView.height, "The Twin Dragons level strip sits under the game view");
await wide.mouse.click(wideBox.x + wideBox.width * 0.9, wideBox.y + wideBox.height / 2);
const centerX = Number(await wide.locator('.area-map[data-map="0"]').getAttribute("data-center-x"));
assert.ok(centerX > 2304 * 0.75, `Clicking near the end of the strip moves the view toward the end of the level (${centerX})`);
assert.deepEqual(wideErrors, []);
console.log("Overview selection reframes widened vertical and horizontal views before centering the selected location.");
await browser.close();
