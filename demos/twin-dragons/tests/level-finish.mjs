// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });

async function open(query) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } }), errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript((key) => {
    localStorage.setItem(key, "seen");
    window.clearingEnds = [];
    const Base = window.Worker;
    window.Worker = class extends Base {
      constructor(...args) {
        super(...args);
        this.addEventListener("message", ({ data }) => {
          if (data?.type !== "batch") return;
          for (const index of data.finishes || []) window.clearingEnds.push(performance.now() + data.motion[index].at(-5) / 0.06);
        });
      }
    };
  }, TOUR_KEY);
  await page.goto(`${url}/${query}`);
  return { page, errors };
}

for (const query of ["?pace=300", "?game=tobu-tobu-girl&pace=0"]) {
  const { page, errors } = await open(query);
  await page.waitForFunction(() => document.querySelector('.map-row[data-current="true"]')?.dataset.map === "1", null, { timeout: 120000 });
  const switched = await page.evaluate(() => performance.now());
  const ends = await page.evaluate(() => window.clearingEnds);
  assert.ok(ends.length, `${query}: the worker marks the rollouts that clear Level 1`);
  assert.ok(switched >= Math.min(...ends), `${query}: the map stays on Level 1 until a clearing rollout has played`);
  assert.deepEqual(errors, []);
  await page.close();
}
console.log("The map holds each cleared level until its clearing rollouts finish playing.");

const { page, errors } = await open("?game=tobu-tobu-girl&pace=0");
const strip = page.locator('.level-overview[data-map="0"]');
await strip.waitFor();
await page.waitForFunction(() => Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 100, null, { timeout: 60000 });
const box = await strip.boundingBox();
await page.mouse.click(box.x + box.width / 2, box.y + box.height * 0.1);
const centerY = Number(await page.locator('.area-map[data-map="0"]').getAttribute("data-center-y"));
assert.ok(Math.abs(centerY - 200) < 20, `Clicking 10% down the level strip centers the view there (${centerY})`);
const view = await page.locator('.area-map[data-map="0"]').boundingBox();
assert.ok(box.x >= view.x + view.width && box.x - (view.x + view.width) < 40, "The level strip sits beside the game view");
assert.equal(await page.locator('#camera-toggle [data-camera="room"]').textContent(), "Manual view");
assert.deepEqual(errors, []);
console.log("Tobu Tobu Girl shows a whole-level strip that moves the view.");
await browser.close();
