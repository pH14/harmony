// SPDX-License-Identifier: AGPL-3.0-or-later
import assert from "node:assert/strict";
import { chromium } from "@playwright/test";
import { TOUR_KEY } from "../src/tour.js";

const url = process.env.DEMO_URL || "http://127.0.0.1:4173";
const browser = await chromium.launch({ headless: true, ...(process.env.CHROME_CHANNEL ? { channel: process.env.CHROME_CHANNEL } : {}) });
const errors = [];
const cases = [
  { game: "twin-dragons", level: 0, keys: ["ArrowRight", "ArrowLeft"] },
  { game: "famidash", level: 21, keys: ["ArrowRight", "ArrowUp", "ArrowDown", "ArrowLeft"] },
  { game: "tobu-tobu-girl", level: 0, keys: ["ArrowUp", "ArrowDown"] },
];
const moves = { ArrowLeft: ["centerX", -1], ArrowRight: ["centerX", 1], ArrowUp: ["centerY", -1], ArrowDown: ["centerY", 1] };
try {
  for (const { game, level, keys } of cases) {
    const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
    page.on("pageerror", (error) => errors.push(`${game}: ${error.message}`));
    await page.addInitScript((key) => localStorage.setItem(key, "seen"), TOUR_KEY);
    await page.goto(`${url}/?game=${game}&level=${level}&pace=0`);
    await page.waitForFunction(() => Number(document.querySelector("#hero-timelines").textContent.replace(/\D/g, "")) > 30, null, { timeout: 60000 });
    await page.locator("#pause").click();
    const view = page.locator(".area-map").first();
    for (const key of keys) {
      const [field, dir] = moves[key], before = Number(await view.evaluate((c, field) => c.dataset[field], field));
      await page.keyboard.down(key);
      await page.waitForTimeout(400);
      await page.keyboard.up(key);
      await page.waitForFunction(([field, dir, before]) => dir * (Number(document.querySelector(".area-map").dataset[field]) - before) > 8, [field, dir, before], { timeout: 5000 });
    }
    assert.equal(await page.locator("#take-control").getAttribute("aria-pressed"), "false", "Arrow keys pan without taking control");
    console.log(`${game}: ${keys.join(", ")} pan the main view.`);
    await page.close();
  }
  assert.deepEqual(errors, []);
} finally {
  await browser.close();
}
